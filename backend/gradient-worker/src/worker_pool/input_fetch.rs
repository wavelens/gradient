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

impl gradient_eval::pool::DownloadSink for DownloadTarget {
    fn transfer(&self, id: u64, done: u64, expected: u64) {
        self.board.transfer(self.index, id, done, expected);
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

    #[test]
    fn the_sink_draws_transfers_on_its_own_row() {
        let board = InputBoard::new(vec![
            ("nixpkgs".into(), InputFetchState::Fetching),
            ("utils".into(), InputFetchState::Fetching),
        ]);
        let second = DownloadTarget {
            board: Arc::clone(&board),
            index: 1,
        };

        gradient_eval::pool::DownloadSink::transfer(&second, 7, 40, 90);

        assert_eq!(board.snapshot()[0].downloaded_bytes, 0);
        assert_eq!(board.snapshot()[1].downloaded_bytes, 40);
        assert_eq!(board.snapshot()[1].expected_bytes, 90);
    }
}
