/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gradient_util::sync::Mutex;
use gradient_wire::types::{InputFetch, InputFetchState};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

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

#[derive(Debug, Default)]
struct NixLogParser {
    transfers: HashMap<u64, DownloadTarget>,
}

impl NixLogParser {
    fn feed(&mut self, line: &str, downloads: &DownloadSlot) -> Option<String> {
        let Some(event) = line
            .strip_prefix("@nix ")
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        else {
            return Some(line.to_owned());
        };
        let id = event["id"].as_u64().unwrap_or_default();
        match event["action"].as_str() {
            Some("start") if event["type"].as_u64() == Some(FILE_TRANSFER) => {
                if let Some(target) = downloads.lock().clone() {
                    self.transfers.insert(id, target);
                }
            }
            Some("stop") => {
                self.transfers.remove(&id);
            }
            Some("result") if event["type"].as_u64() == Some(PROGRESS) => {
                if let Some(target) = self.transfers.get(&id) {
                    let field = |i: usize| event["fields"][i].as_u64().unwrap_or_default();
                    target.board.transfer(target.index, id, field(0), field(1));
                }
            }
            Some("msg") => return Some(event["msg"].as_str().unwrap_or_default().to_owned()),
            _ => {}
        }
        None
    }
}

pub(crate) async fn forward_nix_log(
    mut reader: impl AsyncBufRead + Unpin,
    downloads: DownloadSlot,
    mut print: impl FnMut(&str),
) {
    let mut parser = NixLogParser::default();
    let mut line = Vec::new();
    while matches!(reader.read_until(b'\n', &mut line).await, Ok(read) if read > 0) {
        let text = String::from_utf8_lossy(&line);
        if let Some(text) = parser.feed(text.trim_end_matches(['\r', '\n']), &downloads) {
            print(&text);
        }
        line.clear();
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

    fn slot_on(board: &Arc<InputBoard>, index: usize) -> DownloadSlot {
        Arc::new(Mutex::new(Some(DownloadTarget {
            board: Arc::clone(board),
            index,
        })))
    }

    fn bytes(board: &InputBoard) -> Vec<u64> {
        board
            .snapshot()
            .iter()
            .map(|r| r.downloaded_bytes)
            .collect()
    }

    #[test]
    fn only_file_transfer_activities_count() {
        let board = InputBoard::new(vec![("nixpkgs".into(), InputFetchState::Fetching)]);
        let slot = slot_on(&board, 0);
        let mut parser = NixLogParser::default();
        let lines = [
            r#"@nix {"action":"start","id":7,"type":101,"text":"downloading"}"#,
            r#"@nix {"action":"start","id":8,"type":100,"text":"copying"}"#,
            r#"@nix {"action":"result","id":7,"type":105,"fields":[512,2048,0,0]}"#,
            r#"@nix {"action":"result","id":8,"type":105,"fields":[1,1,0,0]}"#,
        ];
        for line in lines {
            assert_eq!(parser.feed(line, &slot), None);
        }
        assert_eq!(bytes(&board), vec![512]);
        assert_eq!(board.snapshot()[0].expected_bytes, 2048);
        assert_eq!(
            parser.feed(
                r#"@nix {"action":"msg","level":1,"msg":"warning: x"}"#,
                &slot
            ),
            Some("warning: x".into())
        );
        assert_eq!(
            parser.feed("plain tracing line", &slot),
            Some("plain tracing line".into())
        );
    }

    #[test]
    fn a_transfer_stays_with_the_row_it_started_on() {
        let board = InputBoard::new(vec![
            ("nixpkgs".into(), InputFetchState::Done),
            ("utils".into(), InputFetchState::Fetching),
        ]);
        let mut parser = NixLogParser::default();
        parser.feed(
            r#"@nix {"action":"start","id":7,"type":101}"#,
            &slot_on(&board, 0),
        );
        let next = slot_on(&board, 1);
        parser.feed(
            r#"@nix {"action":"result","id":7,"type":105,"fields":[40,90]}"#,
            &next,
        );
        assert_eq!(bytes(&board), vec![40, 0]);
        parser.feed(r#"@nix {"action":"stop","id":7}"#, &next);
        parser.feed(
            r#"@nix {"action":"result","id":7,"type":105,"fields":[90,90]}"#,
            &next,
        );
        assert_eq!(bytes(&board), vec![40, 0]);
    }

    #[tokio::test]
    async fn a_line_that_is_not_utf8_does_not_end_the_log() {
        let board = InputBoard::new(vec![("nixpkgs".into(), InputFetchState::Fetching)]);
        let downloads = DownloadSlot::default();
        *downloads.lock() = Some(DownloadTarget {
            board: Arc::clone(&board),
            index: 0,
        });
        let mut log = b"trace: \xff\xfe\n".to_vec();
        log.extend_from_slice(b"@nix {\"action\":\"start\",\"id\":7,\"type\":101}\n");
        log.extend_from_slice(
            b"@nix {\"action\":\"result\",\"id\":7,\"type\":105,\"fields\":[3,9]}\n",
        );
        let mut printed = Vec::new();
        forward_nix_log(log.as_slice(), downloads, |text| {
            printed.push(text.to_owned())
        })
        .await;
        assert_eq!(printed, vec!["trace: \u{fffd}\u{fffd}".to_owned()]);
        assert_eq!(board.snapshot()[0].downloaded_bytes, 3);
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
