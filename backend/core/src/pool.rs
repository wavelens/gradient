/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use harmonia_protocol::daemon_wire::types2::GCAction;
use harmonia_protocol::types::DaemonStore as _;
use harmonia_store_core::signature::Signature;
use harmonia_store_core::store_path::StorePath;
use harmonia_utils_hash::fmt::CommonHash as _;

use super::executer::{
    BuildOutputInfo, get_build_outputs_from_derivation, get_missing_builds,
    get_pathinfo, nix_store_path,
};

pub use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};

const GCROOTS_DIR: &str = "/nix/var/nix/gcroots/gradient";

/// Gradient's own `PathInfo` — a thin, string-based representation of the
/// fields consumers actually need. Avoids leaking harmonia protocol types
/// throughout the codebase.
#[derive(Debug, Clone)]
pub struct PathInfo {
    pub deriver: Option<String>,
    pub references: Vec<String>,
    /// NAR hash in SRI format, e.g. `sha256-<base64>`.
    pub nar_hash: String,
    pub nar_size: u64,
    pub ultimate: bool,
    pub signatures: Vec<String>,
    pub ca: Option<String>,
}

/// High-level abstraction over a pool of Nix daemon connections.
///
/// Production impl is `LocalNixStoreProvider` (below) which delegates to
/// harmonia's `ConnectionPool` + the existing free functions in `core::executer`.
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

    /// Attaches pre-computed signatures to a store path in the daemon's DB.
    async fn add_signatures(&self, store_path: String, signatures: Vec<Signature>) -> Result<()>;
}

/// Production `NixStoreProvider` backed by harmonia's `ConnectionPool`.
#[derive(Clone)]
pub struct LocalNixStoreProvider {
    pool: ConnectionPool,
}

impl std::fmt::Debug for LocalNixStoreProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalNixStoreProvider").finish_non_exhaustive()
    }
}

impl LocalNixStoreProvider {
    pub fn new(max: usize) -> Self {
        let config = PoolConfig {
            max_size: max,
            ..Default::default()
        };
        Self {
            pool: ConnectionPool::new("/nix/var/nix/daemon-socket/socket", config),
        }
    }

    pub fn pool(&self) -> &ConnectionPool {
        &self.pool
    }
}

#[async_trait]
impl NixStoreProvider for LocalNixStoreProvider {
    async fn query_missing_paths(&self, paths: Vec<String>) -> Result<Vec<String>> {
        get_missing_builds(&self.pool, paths).await
    }

    async fn query_pathinfo(&self, path: String) -> Result<Option<PathInfo>> {
        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for pathinfo: {}", e))?;
        get_pathinfo(nix_store_path(&path), &mut guard).await
    }

    async fn get_build_outputs(&self, derivation_path: String) -> Result<Vec<BuildOutputInfo>> {
        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for build outputs: {}", e))?;
        get_build_outputs_from_derivation(nix_store_path(&derivation_path), &mut guard).await
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
        let sp = StorePath::from_base_path(strip_store_prefix(&store_path))
            .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", store_path, e))?;
        let mut paths_to_delete = std::collections::BTreeSet::new();
        paths_to_delete.insert(sp);
        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for delete_path: {}", e))?;
        let response = guard
            .client()
            .collect_garbage(GCAction::DeleteSpecific, &paths_to_delete, false, 0)
            .await
            .map_err(|e| anyhow::anyhow!("collect_garbage failed: {}", e))?;
        Ok(response.bytes_freed > 0)
    }

    async fn ensure_path(&self, store_path: String) -> Result<()> {
        let sp = StorePath::from_base_path(strip_store_prefix(&store_path))
            .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", store_path, e))?;
        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for ensure_path: {}", e))?;
        let _: () = guard
            .client()
            .ensure_path(&sp)
            .await
            .map_err(|e| anyhow::anyhow!("ensure_path {}: {}", store_path, e))?;
        Ok(())
    }

    async fn add_signatures(&self, store_path: String, signatures: Vec<Signature>) -> Result<()> {
        let sp = StorePath::from_base_path(strip_store_prefix(&store_path))
            .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", store_path, e))?;
        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for add_signatures: {}", e))?;
        let _: () = guard
            .client()
            .add_signatures(&sp, &signatures)
            .await
            .map_err(|e| anyhow::anyhow!("add_signatures {}: {}", store_path, e))?;
        Ok(())
    }
}

/// Convert harmonia's `UnkeyedValidPathInfo` into our local `PathInfo`.
pub fn convert_valid_path_info(
    vi: &harmonia_store_remote::UnkeyedValidPathInfo,
) -> PathInfo {
    PathInfo {
        deriver: vi.deriver.as_ref().map(|d| d.to_string()),
        references: vi.references.iter().map(|r| r.to_string()).collect(),
        nar_hash: format!("{}", vi.nar_hash.as_sri()),
        nar_size: vi.nar_size,
        ultimate: vi.ultimate,
        signatures: vi.signatures.iter().map(|s| s.to_string()).collect(),
        ca: vi.ca.as_ref().map(|ca| ca.to_string()),
    }
}

/// Strips `/nix/store/` prefix, returning just the hash-name component.
fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}
