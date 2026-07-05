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

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use gradient_exec::path_utils::{nix_store_path, strip_store_prefix};
use harmonia_store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};
use tracing::warn;

use gradient_proto::traits::WorkerStore;

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
///
/// Extracted so the policy is asserted in tests without a live daemon.
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

    /// Query the daemon for `store_path`'s direct runtime references.
    ///
    /// Returns canonical `/nix/store/<hash>-<name>` strings. Missing-path or
    /// daemon errors are surfaced as `Err`; the closure walker logs and skips
    /// them so a single flaky path doesn't tank the whole walk.
    async fn query_references(&self, store_path: &str) -> Result<Vec<String>> {
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

        Ok(info
            .references
            .iter()
            .map(|r| nix_store_path(&r.to_string()))
            .collect())
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
                anyhow::anyhow!("add_indirect_root failed for {}: {e}", gcroot_symlink.display())
            })
    }

    /// BFS the runtime reference closure of `seeds` via `query_path_info`.
    ///
    /// Returns every reachable store path including the seeds themselves,
    /// each canonicalised to `/nix/store/<hash>-<name>` form so consumers
    /// (e.g. NAR push) see a single, well-defined string per path.
    /// Paths that fail individual `query_references` calls (e.g. removed
    /// between calls) are logged and skipped - the walk continues so the
    /// caller still gets a best-effort closure for the remaining paths.
    pub async fn collect_runtime_closure(&self, seeds: &[String]) -> HashSet<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        for s in seeds {
            queue.push_back(nix_store_path(s));
        }
        while let Some(path) = queue.pop_front() {
            if !visited.insert(path.clone()) {
                continue;
            }
            match self.query_references(&path).await {
                Ok(refs) => {
                    for r in refs {
                        if !visited.contains(&r) {
                            queue.push_back(r);
                        }
                    }
                }
                Err(e) => {
                    warn!(path = %path, error = %e, "closure walk: skipping unreadable path");
                }
            }
        }
        visited
    }
}

#[async_trait]
impl WorkerStore for LocalNixStore {
    async fn has_path(&self, store_path: &str) -> Result<bool> {
        self.has_path(store_path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the dispatch-time pool exhaustion observed in
    /// production: with `max_concurrent_builds * PREFETCH_CONCURRENCY`
    /// imports queued against the pool, a short acquire deadline fires
    /// before the pool can serve them even though it is making forward
    /// progress. Harmonia's `acquire` now blocks indefinitely, so
    /// [`LocalNixStore::acquire`] bounds the wait with [`POOL_ACQUIRE_TIMEOUT`]
    /// - which must stay generous, anything shorter is an artificial cap
    /// that surfaces as "acquire daemon connection: timed out" mid-build.
    #[test]
    fn pool_config_max_size_and_acquire_timeout() {
        assert_eq!(build_pool_config(8).max_size, 8);
        assert!(
            POOL_ACQUIRE_TIMEOUT >= Duration::from_secs(600),
            "acquire deadline must accommodate worst-case queue depth across \
             concurrent build jobs; got {POOL_ACQUIRE_TIMEOUT:?}"
        );
    }

    /// Regression for `acquire daemon connection: timeout: connecting to
    /// daemon`: `build_pool_config` must override the per-connection
    /// establishment timeout. Harmonia's 10 s default fires while a saturated
    /// local daemon is still completing the handshake for a fresh pooled
    /// connection, failing prefetch imports under high daemon connection count.
    #[test]
    fn pool_config_connection_timeout_is_generous() {
        let cfg = build_pool_config(8);
        assert!(
            cfg.connection_timeout >= Duration::from_secs(120),
            "connection_timeout must tolerate a saturated daemon's handshake; \
             the 10 s harmonia default fails prefetch imports under load; got {:?}",
            cfg.connection_timeout
        );
    }
}
