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
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, StreamExt as _};
use gradient_sources::StoreDaemonError;
use gradient_util::store_path::strip_store_prefix;
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_content_address::{ContentAddress, make_store_path_from_ca};
use harmonia_store_derivation::realisation::{DrvOutput, Realisation, UnkeyedRealisation};
use harmonia_store_path::{StoreDir, StorePath, StorePathName};
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};
use harmonia_utils_hash::{Algorithm, Hash};
use sha2::{Digest as _, Sha256};
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::{debug, warn};

use gradient_wire::traits::WorkerStore;
use gradient_wire::types::Realisation as WireRealisation;
use gradient_worker_client::nar::{PathMeta, PathMetaSource};

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
    metadata_lookup_daemon_connection_semaphore: Arc<Semaphore>,
}

impl LocalNixStore {
    pub fn connect(pool_size: usize) -> Result<Self> {
        Self::connect_at(DEFAULT_DAEMON_SOCKET, pool_size)
    }

    pub fn connect_at(socket_path: &str, pool_size: usize) -> Result<Self> {
        Ok(Self {
            pool: ConnectionPool::new(socket_path, build_pool_config(pool_size)),
            metadata_lookup_daemon_connection_semaphore: Arc::new(Semaphore::new(DAEMON_LOOKUPS)),
        })
    }

    async fn acquire_lookup_permit_and_connection(
        &self,
    ) -> Result<(SemaphorePermit<'_>, PooledConnectionGuard)> {
        let permit = self
            .metadata_lookup_daemon_connection_semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire permit for metadata lookup on daemon: {e}"))?;
        Ok((permit, self.acquire().await?))
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
    pub async fn has_path(&self, store_path: &str) -> Result<bool> {
        let hash_name = strip_store_prefix(store_path);
        let sp = StorePath::from_base_path(hash_name).map_err(|cause| {
            StoreDaemonError::InvalidStorePath {
                path: store_path.to_owned(),
                cause,
            }
        })?;

        let (_permit, mut guard) = self.acquire_lookup_permit_and_connection().await?;
        guard
            .execute(|client| async move { client.is_valid_path(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("is_valid_path failed for {store_path}: {e}"))
    }

    pub async fn local_store_valid_path_hashes(&self) -> Result<Vec<String>> {
        let mut guard = self.acquire().await?;
        let paths = guard
            .execute(|client| async move { client.query_all_valid_paths().await })
            .await
            .map_err(|e| anyhow::anyhow!("query_all_valid_paths failed: {e}"))?;
        Ok(paths.iter().map(|path| path.hash().to_string()).collect())
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
        let sp = StorePath::from_base_path(base).map_err(|cause| {
            StoreDaemonError::InvalidStorePath {
                path: store_path.to_owned(),
                cause,
            }
        })?;

        let (_permit, mut guard) = self.acquire_lookup_permit_and_connection().await?;
        let info = guard
            .execute(|client| async move { client.query_path_info(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed for {store_path}: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!("query_path_info: path not in local store: {store_path}")
            })?;

        Ok(info.nar_size)
    }

    pub async fn import_nar(
        &self,
        info: &ValidPathInfo,
        nar: impl tokio::io::AsyncBufRead + Send + Unpin,
    ) -> Result<()> {
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

    pub async fn register_realisations(&self, realisations: &[WireRealisation]) -> Result<()> {
        for realisation in realisations {
            let realisation = daemon_realisation(realisation)?;
            let mut guard = self.acquire().await?;
            guard
                .execute(|client| async move { client.register_drv_output(&realisation).await })
                .await
                .map_err(|e| anyhow::anyhow!("register_drv_output failed: {e}"))?;
        }

        Ok(())
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

fn parse_store_path(path: &str) -> Result<StorePath> {
    StorePath::from_base_path(strip_store_prefix(path)).map_err(|cause| {
        StoreDaemonError::InvalidStorePath {
            path: path.to_owned(),
            cause,
        }
        .into()
    })
}

fn daemon_realisation(realisation: &WireRealisation) -> Result<Realisation> {
    Ok(Realisation {
        key: DrvOutput {
            drv_path: parse_store_path(&realisation.drv_path)?,
            output_name: realisation
                .output
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid output name {}: {e}", realisation.output))?,
        },
        value: UnkeyedRealisation {
            out_path: parse_store_path(&realisation.path)?,
            signatures: BTreeSet::new(),
        },
    })
}

#[async_trait]
impl WorkerStore for LocalNixStore {
    async fn has_path(&self, store_path: &str) -> Result<bool> {
        self.has_path(store_path).await
    }

    async fn add_nar(&self, name: &str, nar: Vec<u8>) -> Result<String> {
        let info = Self::content_addressed(name, &nar)?;
        let path = info.info.store_dir.display(&info.path).to_string();
        if !self.has_path(&path).await? {
            self.import_nar(&info, nar.as_slice()).await?;
            debug!(%path, bytes = nar.len(), "added NAR to local store");
        }

        Ok(path)
    }

    async fn local_store_valid_path_hashes(&self) -> Result<Vec<String>> {
        self.local_store_valid_path_hashes().await
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

        let (_permit, mut guard) = match self.acquire_lookup_permit_and_connection().await {
            Ok(acquired) => acquired,
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

#[cfg(test)]
mod tests {
    use super::*;
    use harmonia_protocol::daemon::{
        DaemonResult, DaemonStore, FutureResultExt as _, HandshakeDaemonStore, ResultLog,
        TrustLevel,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeNixDaemonConnectionAndQueryCounters {
        connections: AtomicUsize,
        running: AtomicUsize,
        most_running: AtomicUsize,
    }

    struct TestNixDaemonConnectionAnsweringDelayedPathQueriesAndCountingConcurrentQueries(
        Arc<FakeNixDaemonConnectionAndQueryCounters>,
    );

    impl HandshakeDaemonStore
        for TestNixDaemonConnectionAnsweringDelayedPathQueriesAndCountingConcurrentQueries
    {
        type Store = Self;

        fn handshake(self) -> impl ResultLog<Output = DaemonResult<Self>> + Send {
            std::future::ready(Ok(self)).empty_logs()
        }
    }

    impl DaemonStore
        for TestNixDaemonConnectionAnsweringDelayedPathQueriesAndCountingConcurrentQueries
    {
        fn trust_level(&self) -> Option<TrustLevel> {
            None
        }

        fn shutdown(&mut self) -> impl Future<Output = DaemonResult<()>> + Send + '_ {
            std::future::ready(Ok(()))
        }

        fn query_path_info<'a>(
            &'a mut self,
            path: &'a StorePath,
        ) -> impl ResultLog<Output = DaemonResult<Option<UnkeyedValidPathInfo>>> + Send + 'a
        {
            let counters = self.0.clone();
            async move {
                let running = counters.running.fetch_add(1, Ordering::SeqCst) + 1;
                counters.most_running.fetch_max(running, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                counters.running.fetch_sub(1, Ordering::SeqCst);
                let info = LocalNixStore::content_addressed(path.name().as_ref(), b"nar")
                    .expect("content addressed path info");
                Ok(Some(info.info))
            }
            .empty_logs()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_path_metadata_lookups_against_fake_nix_daemon_never_exceed_daemon_lookups_limit_for_running_lookups_or_connections()
     {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let counters = Arc::new(FakeNixDaemonConnectionAndQueryCounters::default());
        let mut connections = tokio::task::JoinSet::new();

        let store = LocalNixStore::connect_at(socket.to_str().unwrap(), 304).unwrap();
        let path = format!("/nix/store/{}-lookup", "b".repeat(32));
        let caller = || futures::future::join_all((0..150).map(|_| store.path_meta(&path)));
        let lookups = async { tokio::join!(caller(), caller()) };
        tokio::pin!(lookups);
        let (first, second) = loop {
            tokio::select! {
                done = &mut lookups => break done,
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    counters.connections.fetch_add(1, Ordering::SeqCst);
                    let handler =
                        TestNixDaemonConnectionAnsweringDelayedPathQueriesAndCountingConcurrentQueries(
                            counters.clone(),
                        );
                    connections.spawn(async move {
                        let (read, write) = tokio::io::split(stream);
                        let _ = harmonia_daemon::server::Builder::new()
                            .set_store_dir(StoreDir::default())
                            .serve_connection(read, write, handler)
                            .await;
                    });
                }
            }
        };

        assert!(first.iter().chain(&second).all(Option::is_some));
        assert!(counters.most_running.load(Ordering::SeqCst) <= DAEMON_LOOKUPS);
        assert!(counters.connections.load(Ordering::SeqCst) <= DAEMON_LOOKUPS);
    }
}
