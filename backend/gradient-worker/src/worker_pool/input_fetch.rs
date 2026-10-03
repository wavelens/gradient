/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use gradient_util::sync::Mutex;
use gradient_wire::types::{InputFetch, InputFetchState};

#[derive(Debug)]
struct InputRow {
    name: String,
    state: InputFetchState,
    transfers: HashMap<u64, (u64, u64)>,
}

#[derive(Debug)]
pub(crate) struct InputBoard {
    rows: Mutex<Vec<InputRow>>,
}

impl InputBoard {
    pub(crate) fn new(rows: Vec<(String, InputFetchState)>) -> Arc<Self> {
        let rows = rows
            .into_iter()
            .map(|(name, state)| InputRow {
                name,
                state,
                transfers: HashMap::new(),
            })
            .collect();
        Arc::new(Self {
            rows: Mutex::new(rows),
        })
    }

    pub(crate) fn set_state(&self, index: usize, state: InputFetchState) {
        self.rows.lock()[index].state = state;
    }

    pub(crate) fn transfer(&self, index: usize, id: u64, done: u64, expected: u64) {
        self.rows.lock()[index]
            .transfers
            .insert(id, (done, expected));
    }

    pub(crate) fn snapshot(&self) -> Vec<InputFetch> {
        self.rows
            .lock()
            .iter()
            .map(|row| {
                let sizes = row.transfers.values();
                let known = sizes.clone().all(|(_, expected)| *expected > 0);
                InputFetch {
                    name: row.name.clone(),
                    state: row.state,
                    downloaded_bytes: sizes.clone().map(|(done, _)| done).sum(),
                    expected_bytes: if known {
                        sizes.map(|(_, expected)| expected).sum()
                    } else {
                        0
                    },
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadTarget {
    pub board: Arc<InputBoard>,
    pub index: usize,
}

pub(crate) type DownloadSlot = Arc<Mutex<Option<DownloadTarget>>>;

const FILE_TRANSFER: u64 = 101;
const PROGRESS: u64 = 105;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NixLine {
    Transfer { id: u64, done: u64, expected: u64 },
    Text(String),
    Quiet,
}

#[derive(Debug, Default)]
pub(crate) struct NixLogParser {
    transfers: HashSet<u64>,
}

impl NixLogParser {
    pub(crate) fn feed(&mut self, line: &str) -> NixLine {
        let Some(json) = line.strip_prefix("@nix ") else {
            return NixLine::Text(line.to_owned());
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(json) else {
            return NixLine::Text(line.to_owned());
        };
        let id = event["id"].as_u64().unwrap_or_default();
        match event["action"].as_str() {
            Some("start") if event["type"].as_u64() == Some(FILE_TRANSFER) => {
                self.transfers.insert(id);
                NixLine::Quiet
            }
            Some("stop") => {
                self.transfers.remove(&id);
                NixLine::Quiet
            }
            Some("result")
                if event["type"].as_u64() == Some(PROGRESS) && self.transfers.contains(&id) =>
            {
                NixLine::Transfer {
                    id,
                    done: event["fields"][0].as_u64().unwrap_or_default(),
                    expected: event["fields"][1].as_u64().unwrap_or_default(),
                }
            }
            Some("msg") => NixLine::Text(event["msg"].as_str().unwrap_or_default().to_owned()),
            _ => NixLine::Quiet,
        }
    }
}

#[async_trait]
pub(crate) trait InputFetcher: Send + Sync {
    async fn fetch_input(
        &self,
        locked: String,
        git_ssh_command: Option<String>,
        target: DownloadTarget,
    ) -> anyhow::Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_file_transfer_activities_count() {
        let mut parser = NixLogParser::default();
        assert_eq!(
            parser.feed(r#"@nix {"action":"start","id":7,"type":101,"text":"downloading"}"#),
            NixLine::Quiet
        );
        assert_eq!(
            parser.feed(r#"@nix {"action":"start","id":8,"type":100,"text":"copying"}"#),
            NixLine::Quiet
        );
        assert_eq!(
            parser.feed(r#"@nix {"action":"result","id":7,"type":105,"fields":[512,2048,0,0]}"#),
            NixLine::Transfer {
                id: 7,
                done: 512,
                expected: 2048
            }
        );
        assert_eq!(
            parser.feed(r#"@nix {"action":"result","id":8,"type":105,"fields":[1,1,0,0]}"#),
            NixLine::Quiet
        );
        assert_eq!(
            parser.feed(r#"@nix {"action":"msg","level":1,"msg":"warning: x"}"#),
            NixLine::Text("warning: x".into())
        );
        assert_eq!(
            parser.feed("plain tracing line"),
            NixLine::Text("plain tracing line".into())
        );
    }

    #[test]
    fn a_row_sums_its_transfers_and_drops_an_unknown_size() {
        let board = InputBoard::new(vec![("nixpkgs".into(), InputFetchState::Queued)]);
        board.set_state(0, InputFetchState::Fetching);
        board.transfer(0, 1, 100, 400);
        board.transfer(0, 2, 50, 100);
        assert_eq!(board.snapshot()[0].downloaded_bytes, 150);
        assert_eq!(board.snapshot()[0].expected_bytes, 500);
        board.transfer(0, 3, 10, 0);
        assert_eq!(board.snapshot()[0].expected_bytes, 0);
        assert_eq!(board.snapshot()[0].state, InputFetchState::Fetching);
    }
}
