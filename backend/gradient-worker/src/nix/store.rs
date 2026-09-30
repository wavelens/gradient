/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Local Nix store wrapper for the worker.
//!
//! Workers build derivations and read store paths via the local nix-daemon.
//! This module wraps harmonia's `ConnectionPool` and exposes only the
//! operations the worker needs: path presence checks, path-info queries,
//! and triggering builds.
//!
//! ## Connection-poisoning policy
//!
//! Every daemon op runs inside [`PooledConnectionGuard::execute`], which
//! discards the connection unless the op returns cleanly - so a cancelled
//! or errored exchange never recycles a mid-protocol socket (which would
//! surface downstream as `"serialised integer N is too large for type 'j'"`
//! or `query_path_info` returning `Ok(None)` for a path that exists).

use std::collections::BTreeSet;
use std::pin::pin;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, StreamExt as _};
use gradient_util::store_path::strip_store_prefix;
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_content_address::{ContentAddress, make_store_path_from_ca};
use harmonia_store_path::{StoreDir, StorePath, StorePathName};
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};
use harmonia_utils_hash::{Algorithm, Hash};
use sha2::{Digest as _, Sha256};
use tracing::{debug, warn};

use gradient_wire::traits::WorkerStore;
use gradient_worker_client::nar::{PathMeta, PathMetaSource};

/// Maximum time `pool.acquire()` blocks before failing with a timeout.
///
/// `add_to_store_nar` legitimately holds a connection for the duration of a
/// NAR upload + daemon ingest, which can run into the tens of seconds for
/// large closures. With concurrent build jobs each issuing parallel
/// prefetch imports, the pool's acquire queue can grow well past the
/// harmonia default of 30 s - long enough that downstream acquires time
/// out spuriously even though the pool is making forward progress.
///
/// 10 minutes mirrors the `HTTP_DOWNLOAD_TIMEOUT` for presigned-URL NAR
/// fetches in `crate::proto::prefetch` - both bound the absolute longest a
/// single import is allowed to take. Any acquire that legitimately needs
/// more than that points at a stuck connection and is the right thing
/// to surface as an error.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(600);

/// Maximum time the pool waits for a brand-new connection to finish its socket
/// connect plus daemon handshake before failing.
///
/// Harmonia's default is 10 s. On a worker whose local nix-daemon already
/// carries a high connection count (concurrent build jobs, their own daemon
/// forks, eval workers), accepting and handshaking a fresh connection under
/// CPU saturation routinely takes longer than that. The pool then surfaces it
/// as `acquire daemon connection: timeout: connecting to daemon` and fails an
/// otherwise-healthy prefetch import. Connection establishment gets the same
/// generous ceiling as [`POOL_ACQUIRE_TIMEOUT`]: any single daemon interaction
/// (queueing, connecting, importing) has 10 minutes before the daemon is
/// treated as genuinely wedged.
const POOL_CONNECT_TIMEOUT: Duration = Duration::from_secs(600);

/// Build the harmonia [`PoolConfig`] used by [`LocalNixStore::connect_at`].
pub(crate) fn build_pool_config(pool_size: usize) -> PoolConfig {
    PoolConfig {
        max_size: pool_size,
        connection_timeout: POOL_CONNECT_TIMEOUT,
        ..Default::default()
    }
}

const DEFAULT_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

/// Thin wrapper around a harmonia `ConnectionPool` for the worker's local nix-daemon.
#[derive(Clone)]
pub struct LocalNixStore {
    pool: ConnectionPool,
}

impl LocalNixStore {
    /// Connect to the local nix-daemon at the default socket path with the given pool size.
    pub fn connect(pool_size: usize) -> Result<Self> {
        Self::connect_at(DEFAULT_DAEMON_SOCKET, pool_size)
    }

    /// Connect to a nix-daemon at a custom socket path with the given pool size.
    pub fn connect_at(socket_path: &str, pool_size: usize) -> Result<Self> {
        Ok(Self {
            pool: ConnectionPool::new(socket_path, build_pool_config(pool_size)),
        })
    }

    /// Acquire a pooled daemon connection, bounding the wait at
    /// [`POOL_ACQUIRE_TIMEOUT`]. Harmonia's `acquire` blocks indefinitely
    /// for a free slot, so the deadline is applied here.
    pub async fn acquire(&self) -> Result<PooledConnectionGuard> {
        tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, self.pool.acquire())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "acquire daemon connection: timed out after {POOL_ACQUIRE_TIMEOUT:?}"
                )
            })?
            .map_err(|e| anyhow::anyhow!("acquire daemon connection: {e}"))
    }

    /// Check whether a store path is present in the local store.
    ///
    /// Uses `is_valid_path` rather than `query_path_info`. The former is the
    /// authoritative "the daemon will accept a dependent that references
    /// this path" check; the latter only confirms the store DB has metadata
    /// for the path, which can disagree with on-disk presence after a GC
    /// race or an interrupted import. A `query_path_info` false-positive
    /// causes the prefetch closure walk to skip a path the daemon will then
    /// reject, surfacing as a confusing `store path '...' does not exist`
    /// error during import of a dependent.
    pub async fn has_path(&self, store_path: &str) -> Result<bool> {
        let hash_name = strip_store_prefix(store_path);
        let sp = StorePath::from_base_path(hash_name)
            .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

        let mut guard = self.acquire().await?;
        guard
            .execute(|client| async move { client.is_valid_path(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("is_valid_path failed for {store_path}: {e}"))
    }

    /// The uncompressed NAR size of each path, `None` when the daemon cannot
    /// report one (a build output before it is built, a path to be fetched).
    pub async fn nar_sizes(&self, store_paths: &[String]) -> Vec<Option<u64>> {
        stream::iter(store_paths.iter().cloned())
            .map(|path| async move {
                self.nar_size(&path)
                    .await
                    .inspect_err(|e| debug!(path = %path, error = %e, "nar size unknown"))
                    .ok()
            })
            .buffered(DAEMON_LOOKUPS)
            .collect()
            .await
    }

    /// `store_path`'s uncompressed NAR size.
    async fn nar_size(&self, store_path: &str) -> Result<u64> {
        let base = strip_store_prefix(store_path);
        let sp = StorePath::from_base_path(base)
            .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

        let mut guard = self.acquire().await?;
        let info = guard
            .execute(|client| async move { client.query_path_info(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed for {store_path}: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!("query_path_info: path not in local store: {store_path}")
            })?;

        Ok(info.nar_size)
    }

    /// Stream `nar` into the daemon as `info` describes it; the transport that
    /// delivered the bytes is what authenticated them, so no signature is checked.
    pub async fn import_nar(&self, info: &ValidPathInfo, nar: &[u8]) -> Result<()> {
        let mut guard = self.acquire().await?;
        guard
            .execute(|client| async move {
                let logs = client.add_to_store_nar(info, nar, false, true);
                let mut logs = pin!(logs);
                while let Some(_msg) = logs.next().await {}
                logs.await
            })
            .await
            .map_err(|e| anyhow::anyhow!("daemon add_to_store_nar({}) failed: {e}", info.path))
    }

    /// The content address of `nar` under `name`, the path every nix fetcher of
    /// the same tree lands on, and the path info that registers it as such.
    fn content_addressed(name: &str, nar: &[u8]) -> Result<ValidPathInfo> {
        let hash = Hash::new(Algorithm::SHA256, &Sha256::digest(nar));
        let store_dir = StoreDir::default();
        let name: StorePathName = name
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid store path name {name}: {e}"))?;
        let path = make_store_path_from_ca(&store_dir, name, ContentAddress::NixArchive(hash));
        let nar_hash = hash
            .try_into()
            .map_err(|e| anyhow::anyhow!("nar hash: {e}"))?;

        Ok(ValidPathInfo {
            path,
            info: UnkeyedValidPathInfo {
                deriver: None,
                nar_hash,
                references: BTreeSet::new(),
                registration_time: None,
                nar_size: nar.len() as u64,
                ultimate: false,
                signatures: BTreeSet::new(),
                ca: Some(ContentAddress::NixArchive(hash)),
                store_dir,
            },
        })
    }

    /// Register `gcroot_symlink` as an indirect GC root with the daemon.
    ///
    /// The caller must have already created the symlink on disk; the daemon
    /// records the link and treats its target as alive for GC purposes
    /// until the link is removed.
    pub async fn add_indirect_root(&self, gcroot_symlink: &std::path::Path) -> Result<()> {
        let bytes = bytes::Bytes::copy_from_slice(gcroot_symlink.as_os_str().as_encoded_bytes());

        let mut guard = self.acquire().await?;
        guard
            .execute(|client| async move { client.add_indirect_root(&bytes).await })
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "add_indirect_root failed for {}: {e}",
                    gcroot_symlink.display()
                )
            })
    }
}

/// Daemon lookups in flight at once; the connection pool bounds them as well.
const DAEMON_LOOKUPS: usize = 32;

#[async_trait]
impl WorkerStore for LocalNixStore {
    async fn has_path(&self, store_path: &str) -> Result<bool> {
        self.has_path(store_path).await
    }

    async fn add_nar(&self, name: &str, nar: Vec<u8>) -> Result<String> {
        let info = Self::content_addressed(name, &nar)?;
        let path = info.info.store_dir.display(&info.path).to_string();
        if !self.has_path(&path).await? {
            self.import_nar(&info, &nar).await?;
            debug!(%path, bytes = nar.len(), "added NAR to local store");
        }

        Ok(path)
    }
}

/// The daemon's view of a path: what a NAR push from this worker confirms with.
/// `None` (logged) when the path is invalid, unknown, or the daemon fails.
#[async_trait]
impl PathMetaSource for LocalNixStore {
    async fn path_meta(&self, store_path: &str) -> Option<PathMeta> {
        let base = strip_store_prefix(store_path);
        let sp = match StorePath::from_base_path(base) {
            Ok(sp) => sp,
            Err(e) => {
                warn!(store_path, error = %e, "gather_path_meta: invalid store path");
                return None;
            }
        };

        let mut guard = match self.acquire().await {
            Ok(g) => g,
            Err(e) => {
                warn!(store_path, error = %e, "gather_path_meta: could not acquire store connection");
                return None;
            }
        };

        let path_info = match guard
            .execute(|client| async move { client.query_path_info(&sp).await })
            .await
        {
            Ok(Some(pi)) => pi,
            Ok(None) => {
                warn!(
                    store_path,
                    "gather_path_meta: path not found in local store"
                );
                return None;
            }
            Err(e) => {
                warn!(
                    store_path,
                    error = %e,
                    "gather_path_meta: query_path_info failed; discarding daemon connection"
                );
                return None;
            }
        };

        let references: Vec<String> = path_info
            .references
            .iter()
            .map(|r: &StorePath| {
                let s = r.to_string();
                s.strip_prefix("/nix/store/").unwrap_or(&s).to_owned()
            })
            .collect();

        let deriver = path_info.deriver.as_ref().map(|d| d.to_string());
        let ca = path_info.ca.as_ref().map(|c| c.to_string());

        Some(PathMeta {
            nar_size: Some(path_info.nar_size),
            references,
            deriver,
            ca,
        })
    }
}
