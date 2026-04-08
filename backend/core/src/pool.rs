/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use nix_daemon::{PathInfo, Progress, Store};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::executer::{
    BuildOutputInfo, get_build_outputs_from_derivation, get_local_store, get_missing_builds,
    get_pathinfo, nix_store_path,
};
use super::types::LocalNixStore;

const GCROOTS_DIR: &str = "/nix/var/nix/gcroots/gradient";

/// High-level abstraction over a pool of Nix daemon connections.
///
/// Production impl is `LocalNixStoreProvider` (below) which delegates to
/// `NixStorePool` + the existing free functions in `core::executer`.
/// Tests use `test_support::fakes::FakeNixStoreProvider`.
#[async_trait]
pub trait NixStoreProvider: Send + Sync + std::fmt::Debug + 'static {
    /// Returns the subset of `paths` (derivation paths or store paths) that
    /// are NOT currently present in the store.
    async fn query_missing_paths(&self, paths: Vec<String>) -> Result<Vec<String>>;

    /// Returns the `PathInfo` for a single store path, or `None` if the path
    /// is not valid.
    async fn query_pathinfo(&self, path: String) -> Result<Option<PathInfo>>;

    /// Enumerates the outputs of a derivation along with their path-info.
    async fn get_build_outputs(&self, derivation_path: String) -> Result<Vec<BuildOutputInfo>>;

    /// Creates a GC root pinning `store_path`. Production impl writes a
    /// symlink under `/nix/var/nix/gcroots/gradient/<name>`.
    async fn add_gcroot(&self, name: String, store_path: String) -> Result<()>;

    /// Removes a GC root previously created via `add_gcroot`. A missing root
    /// is not an error.
    async fn remove_gcroot(&self, name: String) -> Result<()>;

    /// Best-effort deletion of `store_path` from the local Nix store.
    /// Returns `true` if the path was actually deleted, `false` if it could
    /// not be deleted because it is still reachable from a GC root.
    async fn delete_path(&self, store_path: String) -> Result<bool>;

    /// Ensures `store_path` is present in the local Nix store, substituting
    /// it from configured binary caches if it is missing. Used by the
    /// download endpoint to lazily realise outputs of `Substituted` builds
    /// whose data was never copied to the gradient-server's local store.
    async fn ensure_path(&self, store_path: String) -> Result<()>;
}

/// Production `NixStoreProvider` backed by a `NixStorePool` of real Nix daemon
/// connections.
#[derive(Debug)]
pub struct LocalNixStoreProvider {
    pool: NixStorePool,
}

impl LocalNixStoreProvider {
    pub fn new(max: usize) -> Self {
        Self {
            pool: NixStorePool::new(max),
        }
    }
}

#[async_trait]
impl NixStoreProvider for LocalNixStoreProvider {
    async fn query_missing_paths(&self, paths: Vec<String>) -> Result<Vec<String>> {
        get_missing_builds(&self.pool, paths).await
    }

    async fn query_pathinfo(&self, path: String) -> Result<Option<PathInfo>> {
        let mut store = self
            .pool
            .acquire()
            .await
            .context("acquire store for pathinfo")?;
        get_pathinfo(nix_store_path(&path), &mut *store).await
    }

    async fn get_build_outputs(&self, derivation_path: String) -> Result<Vec<BuildOutputInfo>> {
        let mut store = self
            .pool
            .acquire()
            .await
            .context("acquire store for build outputs")?;
        get_build_outputs_from_derivation(nix_store_path(&derivation_path), &mut *store).await
    }

    async fn add_gcroot(&self, name: String, store_path: String) -> Result<()> {
        let gcroot_path = format!("{}/{}", GCROOTS_DIR, name);
        tokio::fs::create_dir_all(GCROOTS_DIR)
            .await
            .with_context(|| format!("create GC roots dir {}", GCROOTS_DIR))?;
        // Remove a stale symlink (best-effort) before re-creating.
        let _ = tokio::fs::remove_file(&gcroot_path).await;
        tokio::fs::symlink(&store_path, &gcroot_path)
            .await
            .with_context(|| format!("create gcroot symlink {}", gcroot_path))?;
        Ok(())
    }

    async fn remove_gcroot(&self, name: String) -> Result<()> {
        let gcroot_path = format!("{}/{}", GCROOTS_DIR, name);
        match tokio::fs::remove_file(&gcroot_path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::Error::from(e).context(format!("remove gcroot {}", gcroot_path))),
        }
    }

    async fn delete_path(&self, store_path: String) -> Result<bool> {
        // The Nix daemon does not expose a `delete-path` operation, so shell
        // out to `nix store delete`. The path is left alive (and we return
        // `false`) when it is still reachable from a GC root.
        let output = tokio::process::Command::new("nix")
            .arg("store")
            .arg("delete")
            .arg(&store_path)
            .output()
            .await
            .with_context(|| format!("spawn nix store delete {}", store_path))?;
        Ok(output.status.success())
    }

    async fn ensure_path(&self, store_path: String) -> Result<()> {
        let mut store = self
            .pool
            .acquire()
            .await
            .context("acquire store for ensure_path")?;
        store
            .ensure_path(nix_store_path(&store_path))
            .result()
            .await
            .with_context(|| format!("ensure_path {}", store_path))
    }
}

/// Connection pool for local Nix daemon connections (Unix socket / subprocess).
///
/// Limits the number of simultaneous open connections via a semaphore.
/// Idle connections are reused to avoid reconnect overhead.
pub struct NixStorePool {
    idle: Arc<Mutex<Vec<LocalNixStore>>>,
    semaphore: Arc<Semaphore>,
}

impl std::fmt::Debug for NixStorePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NixStorePool")
            .field("available_permits", &self.semaphore.available_permits())
            .finish()
    }
}

/// A checked-out connection from `NixStorePool`.
///
/// Implements `DerefMut<Target = LocalNixStore>` so callers can use it
/// as a `&mut LocalNixStore`. Returns the connection to the pool on drop.
pub struct PooledStore {
    store: Option<LocalNixStore>,
    idle: Arc<Mutex<Vec<LocalNixStore>>>,
    _permit: OwnedSemaphorePermit,
}

impl NixStorePool {
    pub fn new(max: usize) -> Self {
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
            semaphore: Arc::new(Semaphore::new(max)),
        }
    }

    /// Acquire a connection, blocking until one is available.
    ///
    /// Returns an idle connection if one exists, otherwise opens a new one.
    pub async fn acquire(&self) -> Result<PooledStore> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("NixStorePool semaphore closed"))?;

        let store = self.idle.lock().unwrap().pop();

        let store = match store {
            Some(s) => s,
            None => get_local_store(None).await?,
        };

        Ok(PooledStore {
            store: Some(store),
            idle: Arc::clone(&self.idle),
            _permit: permit,
        })
    }
}

impl Deref for PooledStore {
    type Target = LocalNixStore;

    fn deref(&self) -> &Self::Target {
        self.store.as_ref().unwrap()
    }
}

impl DerefMut for PooledStore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.store.as_mut().unwrap()
    }
}

impl Drop for PooledStore {
    fn drop(&mut self) {
        if let Some(store) = self.store.take()
            && let Ok(mut idle) = self.idle.lock()
        {
            idle.push(store);
        }
    }
}
