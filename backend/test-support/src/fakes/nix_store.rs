/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use proto::traits::WorkerStore;
use std::collections::HashSet;
use std::sync::Mutex;

/// In-memory fake store suitable for unit tests.
///
/// Tracks which store paths are "present" (built/available). Defaults to empty.
#[derive(Debug, Default)]
pub struct FakeNixStoreProvider {
    present: Mutex<HashSet<String>>,
}

impl FakeNixStoreProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_present_path(self, path: impl Into<String>) -> Self {
        self.present.lock().unwrap().insert(path.into());
        self
    }

    pub fn with_present_paths<I, S>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        {
            let mut p = self.present.lock().unwrap();
            for path in paths {
                p.insert(path.into());
            }
        }
        self
    }

    /// Snapshot of all paths currently present in the store.
    pub fn present_paths(&self) -> HashSet<String> {
        self.present.lock().unwrap().clone()
    }

    /// Remove a path from the store (inverse of `with_present_path`).
    pub fn remove_present_path(&self, path: &str) {
        self.present.lock().unwrap().remove(path);
    }
}

#[async_trait]
impl WorkerStore for FakeNixStoreProvider {
    async fn has_path(&self, store_path: &str) -> Result<bool> {
        Ok(self.present.lock().unwrap().contains(store_path))
    }
}
