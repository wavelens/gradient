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
use harmonia_protocol::types::DaemonStore as _;
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig};

const DEFAULT_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

/// Thin wrapper around a harmonia `ConnectionPool` for the worker's local nix-daemon.
pub struct LocalNixStore {
    pool: ConnectionPool,
}

impl LocalNixStore {
    /// Connect to the local nix-daemon at the default socket path.
    pub async fn connect() -> Result<Self> {
        Self::connect_at(DEFAULT_DAEMON_SOCKET, 4)
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

        let info = guard
            .client()
            .query_path_info(&sp)
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed for {store_path}: {e}"))?;

        Ok(info.is_some())
    }

    /// Return the harmonia connection pool (for build execution).
    pub fn pool(&self) -> &ConnectionPool {
        &self.pool
    }
}

/// Strips `/nix/store/` prefix, returning just the hash-name component.
pub(crate) fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}
