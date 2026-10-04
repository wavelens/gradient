/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every daemon op is executing inside [`PooledConnectionGuard::execute`].
//! The guard is discarding the connection unless the op returned cleanly.
//! A recycled mid-protocol socket would surface as `serialised integer N is too large`.

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

use super::visibility::PathVisibility;

const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(600);

const POOL_CONNECT_TIMEOUT: Duration = Duration::from_secs(600);

pub(crate) fn build_pool_config(pool_size: usize) -> PoolConfig {
    PoolConfig {
        max_size: pool_size,
        connection_timeout: POOL_CONNECT_TIMEOUT,
        ..Default::default()
    }
}

const DEFAULT_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

#[derive(Clone)]
pub struct LocalNixStore {
    pool: ConnectionPool,
    visibility: PathVisibility,
}

impl LocalNixStore {
    pub fn connect(pool_size: usize) -> Result<Self> {
        Self::connect_at(DEFAULT_DAEMON_SOCKET, pool_size)
    }

    pub fn connect_at(socket_path: &str, pool_size: usize) -> Result<Self> {
        Ok(Self {
            pool: ConnectionPool::new(socket_path, build_pool_config(pool_size)),
            visibility: PathVisibility::default(),
        })
    }

    pub fn visibility(&self) -> &PathVisibility {
        &self.visibility
    }

    pub async fn has_path(&self, store_path: &str) -> Result<bool> {
        Ok(self.visibility.allows(store_path) && self.is_on_disk(store_path).await?)
    }

    pub async fn is_hidden(&self, store_path: &str) -> Result<bool> {
        Ok(!self.visibility.allows(store_path) && self.is_on_disk(store_path).await?)
    }

    pub async fn reveal(&self, store_paths: &[String]) -> Result<()> {
        self.visibility.reveal(store_paths).await
    }

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

    /// `is_valid_path` is the authoritative check for a parent the daemon will accept.
    /// `query_path_info` can still report metadata after a GC race or an interrupted import.
    /// That false positive would make the prefetch walk skip a path the daemon then rejects.
    async fn is_on_disk(&self, store_path: &str) -> Result<bool> {
        let hash_name = strip_store_prefix(store_path);
        let sp = StorePath::from_base_path(hash_name)
            .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

        let mut guard = self.acquire().await?;
        guard
            .execute(|client| async move { client.is_valid_path(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("is_valid_path failed for {store_path}: {e}"))
    }

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

    pub async fn import_nar(&self, info: &ValidPathInfo, nar: &[u8]) -> Result<()> {
        let path = info.info.store_dir.display(&info.path).to_string();
        let repair = !self.visibility.allows(&path);
        let mut guard = self.acquire().await?;
        guard
            .execute(|client| async move {
                let logs = client.add_to_store_nar(info, nar, repair, true);
                let mut logs = pin!(logs);
                while let Some(_msg) = logs.next().await {}
                logs.await
            })
            .await
            .map_err(|e| anyhow::anyhow!("daemon add_to_store_nar({}) failed: {e}", info.path))?;
        self.reveal(&[path]).await
    }

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
