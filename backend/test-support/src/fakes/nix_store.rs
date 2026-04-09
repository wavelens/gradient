/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::executer::BuildOutputInfo;
use gradient_core::pool::{NixStoreProvider, PathInfo};
use harmonia_store_core::signature::Signature;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// In-memory `NixStoreProvider` suitable for unit tests.
///
/// Defaults behave as if the store is empty: `query_missing_paths` returns every
/// input path, `query_pathinfo` returns `None`, and `get_build_outputs` returns `[]`.
/// Use the `with_*` builder methods to preload state.
#[derive(Debug, Default)]
pub struct FakeNixStoreProvider {
    /// Paths that *are* present in the store. Anything not in here is "missing".
    present: Mutex<HashSet<String>>,
    pathinfo: Mutex<HashMap<String, PathInfo>>,
    outputs: Mutex<HashMap<String, Vec<BuildOutputInfo>>>,
    /// GC roots: name -> store_path.
    gcroots: Mutex<HashMap<String, String>>,
    /// Store paths passed to `delete_path` (in call order).
    deleted: Mutex<Vec<String>>,
    /// Names that should make `delete_path` return `false` (path "still rooted").
    undeletable: Mutex<HashSet<String>>,
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

    pub fn with_pathinfo(self, path: impl Into<String>, info: PathInfo) -> Self {
        self.pathinfo.lock().unwrap().insert(path.into(), info);
        self
    }

    pub fn with_outputs(
        self,
        derivation: impl Into<String>,
        outputs: Vec<BuildOutputInfo>,
    ) -> Self {
        self.outputs
            .lock()
            .unwrap()
            .insert(derivation.into(), outputs);
        self
    }

    /// Mark a store path as undeletable so `delete_path` returns `Ok(false)`
    /// (simulating "path is still reachable from a GC root").
    pub fn with_undeletable_path(self, store_path: impl Into<String>) -> Self {
        self.undeletable.lock().unwrap().insert(store_path.into());
        self
    }

    /// Snapshot of the currently-installed GC roots (`name -> store_path`).
    pub fn gcroots(&self) -> HashMap<String, String> {
        self.gcroots.lock().unwrap().clone()
    }

    /// Snapshot of all store paths passed to `delete_path`, in order.
    pub fn deleted_paths(&self) -> Vec<String> {
        self.deleted.lock().unwrap().clone()
    }
}

#[async_trait]
impl NixStoreProvider for FakeNixStoreProvider {
    async fn query_missing_paths(&self, paths: Vec<String>) -> Result<Vec<String>> {
        let present = self.present.lock().unwrap();
        Ok(paths.into_iter().filter(|p| !present.contains(p)).collect())
    }

    async fn query_pathinfo(&self, path: String) -> Result<Option<PathInfo>> {
        Ok(self.pathinfo.lock().unwrap().get(&path).cloned())
    }

    async fn get_build_outputs(&self, derivation_path: String) -> Result<Vec<BuildOutputInfo>> {
        Ok(self
            .outputs
            .lock()
            .unwrap()
            .get(&derivation_path)
            .cloned()
            .unwrap_or_default())
    }

    async fn add_gcroot(&self, name: String, store_path: String) -> Result<()> {
        self.gcroots.lock().unwrap().insert(name, store_path);
        Ok(())
    }

    async fn remove_gcroot(&self, name: String) -> Result<()> {
        self.gcroots.lock().unwrap().remove(&name);
        Ok(())
    }

    async fn delete_path(&self, store_path: String) -> Result<bool> {
        let undeletable = self.undeletable.lock().unwrap().contains(&store_path);
        self.deleted.lock().unwrap().push(store_path);
        Ok(!undeletable)
    }

    async fn ensure_path(&self, store_path: String) -> Result<()> {
        self.present.lock().unwrap().insert(store_path);
        Ok(())
    }

    async fn add_signatures(&self, _store_path: String, _signatures: Vec<Signature>) -> Result<()> {
        Ok(())
    }
}
