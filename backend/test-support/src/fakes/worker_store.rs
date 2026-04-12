/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fake [`WorkerStore`] for testing worker executor code without a real nix-daemon.

use anyhow::Result;
use async_trait::async_trait;
use proto::traits::WorkerStore;
use std::collections::HashSet;
use std::sync::Mutex;

/// In-memory [`WorkerStore`] backed by a set of present paths.
///
/// `has_path()` returns `true` if the path was added via `with_present_path`.
#[derive(Debug, Default)]
pub struct FakeWorkerStore {
    present: Mutex<HashSet<String>>,
}

impl FakeWorkerStore {
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
        let mut p = self.present.lock().unwrap();
        for path in paths {
            p.insert(path.into());
        }
        drop(p);
        self
    }

    /// Build from a `FakeNixStoreProvider`'s present paths.
    pub fn from_present_paths(paths: HashSet<String>) -> Self {
        Self {
            present: Mutex::new(paths),
        }
    }
}

#[async_trait]
impl WorkerStore for FakeWorkerStore {
    async fn has_path(&self, store_path: &str) -> Result<bool> {
        Ok(self.present.lock().unwrap().contains(store_path))
    }
}
