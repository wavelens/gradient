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

use anyhow::Result;
use async_trait::async_trait;
use harmonia_protocol::types::{DaemonError, DaemonErrorKind};
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig};

use proto::traits::WorkerStore;

/// Decide whether a daemon error leaves the pooled connection in an unusable
/// state. Only clean server-side `Remote` errors are safe to recover from —
/// any other variant may have left bytes in the transport buffer (or an
/// abandoned write frame) that would corrupt the next caller's protocol
/// stream.
///
/// We surface this so daemon call sites can [`PooledConnectionGuard::mark_broken`]
/// their guard and force the pool to discard the connection instead of
/// handing it to the next acquirer.
pub fn is_connection_corrupt(err: &DaemonError) -> bool {
    !matches!(err.kind(), DaemonErrorKind::Remote(_))
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
        let config = PoolConfig {
            max_size: pool_size,
            ..Default::default()
        };
        Ok(Self {
            pool: ConnectionPool::new(socket_path, config),
        })
    }

    /// Check whether a store path is present in the local store.
    pub async fn has_path(&self, store_path: &str) -> Result<bool> {
        let hash_name = strip_store_prefix(store_path);
        let sp = StorePath::from_base_path(hash_name)
            .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

        let mut guard = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire store for has_path: {e}"))?;

        match guard.client().query_path_info(&sp).await {
            Ok(info) => Ok(info.is_some()),
            Err(e) => {
                let corrupt = is_connection_corrupt(&e);
                let err = anyhow::anyhow!("query_path_info failed for {store_path}: {e}");
                if corrupt {
                    guard.mark_broken();
                }
                Err(err)
            }
        }
    }

    /// Return the harmonia connection pool (for build execution).
    pub fn pool(&self) -> &ConnectionPool {
        &self.pool
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
    use harmonia_protocol::log::Verbosity;
    use harmonia_protocol::types::{DaemonInt, DaemonString, RemoteError};

    fn remote_err() -> DaemonError {
        DaemonError::from(RemoteError {
            level: Verbosity::Error,
            msg: DaemonString::from(b"some build failed".to_vec()),
            exit_status: DaemonInt::default(),
            traces: vec![],
        })
    }

    #[test]
    fn remote_errors_are_recoverable() {
        // Daemon-side errors (e.g. "build failed") leave the protocol stream
        // aligned, so the pooled connection is safe to reuse.
        assert!(!is_connection_corrupt(&remote_err()));
    }

    #[test]
    fn io_errors_mark_connection_corrupt() {
        // Transport-level IO errors can leave half-written frames in the
        // buffer — reusing the connection would desync the next caller.
        let io = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "broken pipe");
        let err = DaemonError::from(io);
        assert!(is_connection_corrupt(&err));
    }

    #[test]
    fn custom_errors_are_treated_as_corrupt() {
        // We can't distinguish a framing bug from anything else when the
        // error surfaces as a custom string, so we play it safe and drop the
        // connection.
        let err = DaemonError::custom("parse error L, non-absolute store path \"L\"");
        assert!(is_connection_corrupt(&err));
    }
}
