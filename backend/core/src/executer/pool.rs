/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use harmonia_protocol::daemon_wire::types2::GCAction;
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_core::signature::Signature;
use harmonia_store_core::store_path::StorePath;
use harmonia_utils_hash::fmt::CommonHash as _;
use std::collections::HashMap;

use crate::executer::path_utils::nix_store_path;
use crate::sources::get_hash_from_path;

pub use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};

const GCROOTS_DIR: &str = "/nix/var/nix/gcroots/gradient";

// ── Nix store helper functions ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BuildOutputInfo {
    pub name: String,
    pub path: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
}

pub async fn get_missing_builds(pool: &ConnectionPool, paths: Vec<String>) -> Result<Vec<String>> {
    let mut output_paths: HashMap<String, String> = HashMap::new();
    let mut drv_paths: Vec<String> = Vec::new();

    for path in paths {
        if path.ends_with(".drv") {
            drv_paths.push(path);
        } else {
            output_paths.insert(path.clone(), nix_store_path(&path));
        }
    }

    if !drv_paths.is_empty() {
        let mut tasks: FuturesUnordered<_> = drv_paths
            .into_iter()
            .map(|path| async move {
                let mut guard = pool
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("acquire store for output map: {}", e))?;
                let full_path = nix_store_path(&path);
                let output_map = get_output_paths_internal(full_path.clone(), guard.client())
                    .await
                    .with_context(|| format!("Failed to get output path for {}", full_path))?;
                anyhow::Ok((path, output_map))
            })
            .collect();

        while let Some(result) = tasks.next().await {
            let (path, output_map) = result?;
            for out_path in output_map.values() {
                output_paths.insert(path.clone(), out_path.clone());
            }
        }
    }

    let mut guard = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire store for valid paths: {}", e))?;

    let store_paths: harmonia_store_core::store_path::StorePathSet = output_paths
        .values()
        .filter_map(|p| StorePath::from_base_path(strip_store_prefix(p)).ok())
        .collect();

    let valid_paths = guard
        .client()
        .query_valid_paths(&store_paths, true)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query valid paths: {}", e))?;

    let valid_strings: std::collections::HashSet<String> = valid_paths
        .iter()
        .map(|p| format!("/nix/store/{}", p))
        .collect();

    let missing = output_paths
        .into_iter()
        .filter(|(_, v)| !valid_strings.contains(v))
        .map(|(k, _)| k)
        .collect();

    Ok(missing)
}

async fn get_output_paths_internal<R, W>(
    path: String,
    store: &mut harmonia_store_remote::DaemonClient<R, W>,
) -> Result<HashMap<String, String>>
where
    R: tokio::io::AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    let store_path = StorePath::from_base_path(strip_store_prefix(&path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", path, e))?;
    let output_map = store
        .query_derivation_output_map(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map failed: {}", e))?;
    Ok(output_map
        .into_iter()
        .filter_map(|(name, sp_opt)| sp_opt.map(|sp| (name.to_string(), format!("/nix/store/{}", sp))))
        .collect())
}

pub async fn get_pathinfo(path: String, guard: &mut PooledConnectionGuard) -> Result<Option<PathInfo>> {
    let store_path = StorePath::from_base_path(strip_store_prefix(&path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", path, e))?;
    let info = guard
        .client()
        .query_path_info(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?;
    Ok(info.map(|vi| convert_valid_path_info(&vi)))
}

pub async fn get_build_outputs_from_derivation(
    derivation_path: String,
    guard: &mut PooledConnectionGuard,
) -> Result<Vec<BuildOutputInfo>> {
    let drv_store_path = StorePath::from_base_path(strip_store_prefix(&derivation_path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", derivation_path, e))?;
    let output_map = guard
        .client()
        .query_derivation_output_map(&drv_store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map failed: {}", e))?;

    let mut outputs = Vec::new();
    for (output_name, output_store_path_opt) in &output_map {
        let Some(output_store_path) = output_store_path_opt else { continue };
        let output_path_str = format!("/nix/store/{}", output_store_path);
        if let Some(vi) = guard
            .client()
            .query_path_info(output_store_path)
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?
        {
            let (hash, package) = get_hash_from_path(output_path_str.clone())
                .with_context(|| format!("Failed to parse path {}", output_path_str))?;
            outputs.push(BuildOutputInfo {
                name: output_name.to_string(),
                path: output_path_str,
                hash,
                package,
                ca: vi.ca.as_ref().map(|ca| ca.to_string()),
            });
        }
    }
    Ok(outputs)
}

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
        nar_hash: format!("{}", vi.nar_hash.sri()),
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
