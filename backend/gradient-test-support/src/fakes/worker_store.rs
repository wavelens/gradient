/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_wire::traits::WorkerStore;
use std::collections::HashSet;
use std::sync::Mutex;

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

    async fn add_nar(&self, name: &str, nar: Vec<u8>) -> Result<String> {
        let path = fake_nar_path(name, &nar);
        self.present.lock().unwrap().insert(path.clone());
        Ok(path)
    }
}

/// The path is following from the bytes like the daemon's would, without nix hashing. The hash slot
/// is holding the NAR length and a byte sum.
pub fn fake_nar_path(name: &str, nar: &[u8]) -> String {
    let sum: u64 = nar.iter().map(|b| u64::from(*b)).sum();
    format!("/nix/store/{:016x}{:016x}-{name}", nar.len(), sum)
}
