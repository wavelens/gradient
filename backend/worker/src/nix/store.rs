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
//! Any daemon call that returns `Err` marks its guard with
//! [`PooledConnectionGuard::mark_broken`] so the connection is dropped
//! instead of recycled. Previously we tried to distinguish "clean" remote
//! errors from transport errors and reuse the former, but in practice an
//! errored connection sometimes leaves the protocol stream out of phase —
//! the next acquirer then reads garbage and surfaces it as a confusing
//! `"serialised integer N is too large for type 'j'"` or as
//! `query_path_info` returning `Ok(None)` for a path that exists. The
//! cost of a fresh handshake is far smaller than a poisoned pool, so the
//! rule is now blanket: error in → connection out.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig};
use tracing::warn;

use proto::traits::WorkerStore;

/// Maximum time `pool.acquire()` blocks before failing with a timeout.
///
/// `add_to_store_nar` legitimately holds a connection for the duration of a
/// NAR upload + daemon ingest, which can run into the tens of seconds for
/// large closures. With concurrent build jobs each issuing parallel
/// prefetch imports, the pool's acquire queue can grow well past the
/// harmonia default of 30 s — long enough that downstream acquires time
/// out spuriously even though the pool is making forward progress.
///
/// 10 minutes mirrors the `HTTP_DOWNLOAD_TIMEOUT` for presigned-URL NAR
/// fetches in `proto::nar_import` — both bound the absolute longest a
/// single import is allowed to take. Any acquire that legitimately needs
/// more than that points at a stuck connection and is the right thing
/// to surface as an error.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(600);

/// Build the harmonia [`PoolConfig`] used by [`LocalNixStore::connect_at`].
///
/// Extracted so the policy is asserted in tests without a live daemon.
pub(crate) fn build_pool_config(pool_size: usize) -> PoolConfig {
    PoolConfig {
        max_size: pool_size,
        acquire_timeout: POOL_ACQUIRE_TIMEOUT,
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

        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for has_path: {e}"))?;

        match guard.client().is_valid_path(&sp).await {
            Ok(valid) => Ok(valid),
            Err(e) => {
                guard.mark_broken();
                Err(anyhow::anyhow!("is_valid_path failed for {store_path}: {e}"))
            }
        }
    }

    /// Return the harmonia connection pool (for build execution).
    pub fn pool(&self) -> &ConnectionPool {
        &self.pool
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

        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for query_references: {e}"))?;

        let info = match guard.client().query_path_info(&sp).await {
            Ok(Some(pi)) => pi,
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "query_path_info: path not in local store: {store_path}"
                ));
            }
            Err(e) => {
                guard.mark_broken();
                return Err(anyhow::anyhow!(
                    "query_path_info failed for {store_path}: {e}"
                ));
            }
        };

        Ok(info
            .references
            .iter()
            .map(|r| canonicalize_store_path(&r.to_string()))
            .collect())
    }

    /// BFS the runtime reference closure of `seeds` via `query_path_info`.
    ///
    /// Returns every reachable store path including the seeds themselves,
    /// each canonicalised to `/nix/store/<hash>-<name>` form so consumers
    /// (e.g. NAR push) see a single, well-defined string per path.
    /// Paths that fail individual `query_references` calls (e.g. removed
    /// between calls) are logged and skipped — the walk continues so the
    /// caller still gets a best-effort closure for the remaining paths.
    pub async fn collect_runtime_closure(&self, seeds: &[String]) -> HashSet<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        for s in seeds {
            queue.push_back(canonicalize_store_path(s));
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

/// Normalise to absolute `/nix/store/<hash>-<name>`. Bare hash-name input is
/// prefixed; already-absolute input is left as-is.
fn canonicalize_store_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/nix/store/{}", path)
    }
}

#[async_trait]
impl WorkerStore for LocalNixStore {
    async fn has_path(&self, store_path: &str) -> Result<bool> {
        self.has_path(store_path).await
    }
}

/// Strips `/nix/store/` prefix, returning just the hash-name component.
pub(crate) fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the dispatch-time pool exhaustion observed in
    /// production: with `max_concurrent_builds * PREFETCH_CONCURRENCY`
    /// imports queued against the pool, the harmonia default
    /// `acquire_timeout` of 30 s fires before the pool can serve them
    /// even though it is making forward progress. We override it to
    /// 10 min — anything shorter is an artificial cap that surfaces as
    /// "acquire local store for import: timeout" mid-build.
    #[test]
    fn pool_config_acquire_timeout_is_generous() {
        let cfg = build_pool_config(8);
        assert_eq!(cfg.max_size, 8);
        assert!(
            cfg.acquire_timeout >= Duration::from_secs(600),
            "acquire_timeout must accommodate worst-case queue depth across \
             concurrent build jobs; got {:?}",
            cfg.acquire_timeout
        );
    }

    #[test]
    fn canonicalize_store_path_prefixes_bare_hash_name() {
        assert_eq!(
            canonicalize_store_path("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"
        );
    }

    #[test]
    fn canonicalize_store_path_preserves_absolute() {
        assert_eq!(
            canonicalize_store_path("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo"
        );
    }
}
