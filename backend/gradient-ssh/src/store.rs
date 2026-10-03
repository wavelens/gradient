/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use futures::{Stream, StreamExt as _};
use gradient_daemon::backend::{Backend, ConnInfo};
use gradient_daemon::journal::Journal;
use gradient_db::cache_paths::{ServedPath, served_hashes, served_path};
use gradient_util::nix_hash::normalize_nar_hash;
use harmonia_protocol::daemon::wire::types2::{BuildMode, KeyedBuildResult, QueryMissingResult};
use harmonia_protocol::daemon::{
    AddToStoreItem, DaemonError, DaemonResult, DaemonStore, FutureResultExt as _,
    HandshakeDaemonStore, ResultLog, ResultLogExt as _, TrustLevel,
};
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_derivation::derived_path::{DerivedPath, SingleDerivedPath};
use harmonia_store_path::{StoreDir, StorePath, StorePathHash, StorePathSet};
use harmonia_store_path_info::NarHash;
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_signature::Signature;
use std::fmt::Display;
use std::future::{Future, ready};
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::AsyncBufRead;
use tokio::task::JoinSet;

pub struct SshBackend {
    session: Arc<Session>,
    journal: Journal,
}

impl SshBackend {
    pub fn new(session: Arc<Session>) -> Self {
        Self {
            session,
            journal: Journal::new(),
        }
    }
}

impl Backend for SshBackend {
    type Handler = CacheStore;

    fn journal(&self) -> &Journal {
        &self.journal
    }

    fn handler(self: &Arc<Self>, _conn: ConnInfo) -> CacheStore {
        CacheStore {
            session: self.session.clone(),
        }
    }

    fn control(
        &self,
        _cmd: &str,
        _args: &serde_json::Value,
    ) -> Option<anyhow::Result<serde_json::Value>> {
        None
    }
}

pub struct CacheStore {
    session: Arc<Session>,
}

const CONCURRENT_COMMITS: usize = 8;

fn joined(result: Option<Result<anyhow::Result<()>, tokio::task::JoinError>>) -> DaemonResult<()> {
    match result {
        Some(Ok(committed)) => committed.map_err(err),
        Some(Err(e)) => Err(err(e)),
        None => Ok(()),
    }
}

pub(crate) fn err(e: impl Display) -> DaemonError {
    DaemonError::custom(e.to_string())
}

fn parse_store_path(path: &str) -> anyhow::Result<StorePath> {
    let base = path.strip_prefix("/nix/store/").unwrap_or(path);
    StorePath::from_base_path(base).map_err(|e| anyhow::anyhow!("{base}: {e}"))
}

impl CacheStore {
    async fn served(&self, path: &StorePath) -> DaemonResult<Option<ServedPath>> {
        served_path(
            &self.session.state.web_db,
            &self.session.caches,
            &path.hash().to_string(),
        )
        .await
        .map_err(err)
    }

    async fn copy_in(
        &self,
        info: &ValidPathInfo,
        reader: impl AsyncBufRead + Send + Unpin,
    ) -> DaemonResult<()> {
        if self.served(&info.path).await?.is_some() {
            tokio::io::copy(&mut std::pin::pin!(reader), &mut tokio::io::sink()).await?;
            return Ok(());
        }

        crate::ingest::import(&self.session, info, reader)
            .await
            .map_err(err)
    }

    async fn path_info(&self, row: ServedPath) -> anyhow::Result<UnkeyedValidPathInfo> {
        let state = &self.session.state;
        let nar_hash = row
            .nar_hash
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("{} has no NAR hash", row.hash))?;
        let nar_hash = normalize_nar_hash(nar_hash)
            .parse::<Any<NarHash>>()
            .map_err(|e| anyhow::anyhow!("{}: {e}", row.hash))?
            .into_hash();

        let references =
            gradient_db::graph::runtime_closure::references_for_hash(&state.web_db, &row.hash)
                .await?
                .iter()
                .map(|r| parse_store_path(r))
                .collect::<anyhow::Result<_>>()?;

        let signatures = row
            .signatures
            .iter()
            .filter_map(|(cache, sig)| {
                gradient_sources::full_signature_token(sig, &state.config.server.serve_url, cache)
                    .parse::<Signature>()
                    .ok()
            })
            .collect();

        Ok(UnkeyedValidPathInfo {
            deriver: row.deriver.as_deref().map(parse_store_path).transpose()?,
            nar_hash,
            references,
            registration_time: None,
            nar_size: row.nar_size.unwrap_or_default() as u64,
            ultimate: false,
            signatures,
            ca: row.ca.as_deref().and_then(|ca| ca.parse().ok()),
            store_dir: StoreDir::default(),
        })
    }
}

impl HandshakeDaemonStore for CacheStore {
    type Store = Self;

    fn handshake(self) -> impl ResultLog<Output = DaemonResult<Self::Store>> + Send {
        ready(Ok(self)).empty_logs()
    }
}

impl DaemonStore for CacheStore {
    fn trust_level(&self) -> Option<TrustLevel> {
        Some(TrustLevel::NotTrusted)
    }

    fn is_valid_path<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<bool>> + Send + 'a {
        async move { Ok(self.served(path).await?.is_some()) }.empty_logs()
    }

    fn query_valid_paths<'a>(
        &'a mut self,
        paths: &'a StorePathSet,
        _substitute: bool,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + 'a {
        async move {
            let hashes: Vec<String> = paths.iter().map(|p| p.hash().to_string()).collect();
            let served = served_hashes(&self.session.state.web_db, &self.session.caches, &hashes)
                .await
                .map_err(err)?;

            Ok(paths
                .iter()
                .filter(|p| served.contains(&p.hash().to_string()))
                .cloned()
                .collect())
        }
        .empty_logs()
    }

    fn query_path_info<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<Option<UnkeyedValidPathInfo>>> + Send + 'a {
        async move {
            let Some(row) = self.served(path).await? else {
                return Ok(None);
            };

            self.path_info(row).await.map(Some).map_err(err)
        }
        .empty_logs()
    }

    fn query_path_from_hash_part<'a>(
        &'a mut self,
        hash: &'a StorePathHash,
    ) -> impl ResultLog<Output = DaemonResult<Option<StorePath>>> + Send + 'a {
        async move {
            let row = served_path(
                &self.session.state.web_db,
                &self.session.caches,
                &hash.to_string(),
            )
            .await
            .map_err(err)?;

            row.map(|r| parse_store_path(&format!("{}-{}", r.hash, r.package)).map_err(err))
                .transpose()
        }
        .empty_logs()
    }

    fn nar_from_path<'s>(
        &'s mut self,
        path: &'s StorePath,
    ) -> impl ResultLog<Output = DaemonResult<impl AsyncBufRead + Send + use<>>> + Send + 's {
        async move {
            if self.served(path).await?.is_none() {
                return Err(err(format!("{path} is not in the project caches")));
            }

            crate::nar::open_raw(&self.session.state, &path.hash().to_string())
                .await
                .map_err(err)?
                .ok_or_else(|| err(format!("{path} has no stored NAR")))
        }
        .empty_logs()
    }

    fn add_to_store_nar<'s, 'r, 'i, R>(
        &'s mut self,
        info: &'i ValidPathInfo,
        source: R,
        _repair: bool,
        _dont_check_sigs: bool,
    ) -> Pin<Box<dyn ResultLog<Output = DaemonResult<()>> + Send + 'r>>
    where
        R: AsyncBufRead + Send + Unpin + 'r,
        's: 'r,
        'i: 'r,
    {
        async move { self.copy_in(info, source).await }
            .empty_logs()
            .boxed_result()
    }

    fn add_multiple_to_store<'s, 'i, 'r, S, R>(
        &'s mut self,
        _repair: bool,
        _dont_check_sigs: bool,
        stream: S,
    ) -> Pin<Box<dyn ResultLog<Output = DaemonResult<()>> + Send + 'r>>
    where
        S: Stream<Item = Result<AddToStoreItem<R>, DaemonError>> + Send + 'i,
        R: AsyncBufRead + Send + Unpin + 'i,
        's: 'r,
        'i: 'r,
    {
        async move {
            let mut stream = std::pin::pin!(stream);
            let mut commits = JoinSet::new();
            while let Some(item) = stream.next().await {
                let AddToStoreItem { info, reader } = item?;
                if self.served(&info.path).await?.is_some() {
                    tokio::io::copy(&mut std::pin::pin!(reader), &mut tokio::io::sink()).await?;
                    continue;
                }

                let pending = crate::ingest::stage(&self.session, &info, reader)
                    .await
                    .map_err(err)?;
                let session = self.session.clone();
                commits.spawn(async move { crate::ingest::commit(&session, &info, pending).await });
                while commits.len() >= CONCURRENT_COMMITS {
                    joined(commits.join_next().await)?;
                }
            }

            while let Some(result) = commits.join_next().await {
                joined(Some(result))?;
            }

            Ok(())
        }
        .empty_logs()
        .boxed_result()
    }

    fn ensure_path<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        async move {
            match self.served(path).await? {
                Some(_) => Ok(()),
                None => Err(err(format!("{path} is not in the project caches"))),
            }
        }
        .empty_logs()
    }

    fn build_paths<'a>(
        &'a mut self,
        drvs: &'a [DerivedPath],
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        crate::daemon_build::build_paths(self.session.clone(), drvs.to_vec()).and_then(
            |results| async move {
                match results
                    .into_iter()
                    .find_map(|r| r.result.failure().map(|f| f.error_msg.clone()))
                {
                    Some(message) => Err(err(String::from_utf8_lossy(&message))),
                    None => Ok(()),
                }
            },
        )
    }

    fn build_paths_with_results<'a>(
        &'a mut self,
        drvs: &'a [DerivedPath],
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<Vec<KeyedBuildResult>>> + Send + 'a {
        crate::daemon_build::build_paths(self.session.clone(), drvs.to_vec())
    }

    fn query_missing<'a>(
        &'a mut self,
        paths: &'a [DerivedPath],
    ) -> impl ResultLog<Output = DaemonResult<QueryMissingResult>> + Send + 'a {
        async move {
            let mut missing = QueryMissingResult {
                will_build: StorePathSet::new(),
                will_substitute: StorePathSet::new(),
                unknown: StorePathSet::new(),
                download_size: 0,
                nar_size: 0,
            };
            for path in paths {
                match path {
                    DerivedPath::Built { drv_path, .. } => {
                        if let SingleDerivedPath::Opaque(drv) = drv_path.as_ref() {
                            missing.will_build.insert(drv.clone());
                        }
                    }
                    DerivedPath::Opaque(p) => {
                        if self.served(p).await?.is_none() {
                            missing.unknown.insert(p.clone());
                        }
                    }
                }
            }

            Ok(missing)
        }
        .empty_logs()
    }

    fn add_temp_root<'a>(
        &'a mut self,
        _path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        ready(Ok(())).empty_logs()
    }

    fn shutdown(&mut self) -> impl Future<Output = DaemonResult<()>> + Send + '_ {
        ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_daemon::server::{next_conn, serve_stream};
    use gradient_types::CacheId;
    use harmonia_protocol::daemon::wire::types2::{BuildResultInner, SuccessStatus};
    use harmonia_store_path::StorePath;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, Value};
    use std::collections::BTreeMap;
    use tokio::task::JoinSet;

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    type TestClient = harmonia_store_remote::DaemonClient<
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    >;

    fn session_over(db: DatabaseConnection) -> Arc<Session> {
        Arc::new(Session {
            state: gradient_test_support::state::test_state_web(db),
            user: gradient_test_support::fixtures::user(),
            project: gradient_test_support::fixtures::project(),
            permissions: 0,
            caches: vec![CacheId::now_v7()],
            evaluation: Default::default(),
        })
    }

    async fn connect(session: Arc<Session>) -> (JoinSet<()>, TestClient) {
        let backend = Arc::new(SshBackend::new(session));
        let (client_side, server_side) = tokio::io::duplex(1 << 20);
        let mut server = JoinSet::new();
        server.spawn(serve_stream(backend, next_conn(None), server_side));
        let (read, write) = tokio::io::split(client_side);
        let client = harmonia_store_remote::DaemonClient::builder()
            .connect(read, write)
            .await
            .expect("handshake");
        (server, client)
    }

    fn served_row() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("hash".to_owned(), Value::from(HASH)),
            ("package".to_owned(), Value::from("hello")),
            (
                "nar_hash".to_owned(),
                Value::from(Some(format!("sha256:{}", "00".repeat(32)))),
            ),
            ("nar_size".to_owned(), Value::from(Some(42_i64))),
            ("deriver".to_owned(), Value::from(None::<String>)),
            ("ca".to_owned(), Value::from(None::<String>)),
            ("cache_name".to_owned(), Value::from("main")),
            ("signature".to_owned(), Value::from(vec![7_u8; 64])),
        ])
    }

    fn references_row() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("hash".to_owned(), Value::from(HASH)),
            (
                "references".to_owned(),
                Value::from(Some(format!("{HASH}-hello"))),
            ),
        ])
    }

    fn hello() -> StorePath {
        StorePath::from_base_path(&format!("{HASH}-hello")).expect("path")
    }

    async fn nar_of(contents: &[u8]) -> Vec<u8> {
        use futures::TryStreamExt as _;
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(file.path(), contents).expect("write");
        let chunks: Vec<_> = harmonia_file_nar::NarByteStream::new(file.path().to_path_buf())
            .try_collect()
            .await
            .expect("dump NAR");
        chunks.concat()
    }

    #[tokio::test]
    async fn path_info_comes_from_the_project_caches() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![served_row()]])
            .append_query_results([vec![references_row()]])
            .into_connection();
        let (_server, mut client) = connect(session_over(db)).await;

        let info = client
            .query_path_info(&hello())
            .await
            .expect("info")
            .expect("served");
        assert_eq!(info.nar_size, 42);
        assert_eq!(info.signatures.len(), 1);
        assert!(info.references.contains(&hello()));
    }

    #[tokio::test]
    async fn a_path_outside_the_project_caches_is_invalid() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (_server, mut client) = connect(session_over(db)).await;

        assert!(!client.is_valid_path(&hello()).await.expect("valid"));
        assert!(
            client
                .query_path_info(&hello())
                .await
                .expect("info")
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_client_is_not_trusted() {
        let session = session_over(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let backend = Arc::new(SshBackend::new(session));
        let store = backend.handler(next_conn(None));
        assert_eq!(store.trust_level(), Some(TrustLevel::NotTrusted));
    }

    #[tokio::test]
    async fn garbage_collection_is_refused() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let (_server, mut client) = connect(session_over(db)).await;

        let err = client.find_roots().await.expect_err("refused");
        assert!(err.to_string().contains("unimplemented"), "{err}");
    }

    #[tokio::test]
    async fn copying_in_a_served_path_imports_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![served_row()]])
            .into_connection();
        let (_server, mut client) = connect(session_over(db)).await;
        let nar = nar_of(b"already there").await;
        let info = ValidPathInfo {
            path: hello(),
            info: UnkeyedValidPathInfo {
                deriver: None,
                nar_hash: NarHash::digest(&nar),
                references: Default::default(),
                registration_time: None,
                nar_size: nar.len() as u64,
                ultimate: false,
                signatures: Default::default(),
                ca: None,
                store_dir: StoreDir::default(),
            },
        };

        client
            .add_multiple_to_store(
                false,
                true,
                futures::stream::iter([Ok(AddToStoreItem {
                    info,
                    reader: std::io::Cursor::new(nar),
                })]),
            )
            .await
            .expect("skipped without TriggerEvaluation");
    }

    #[tokio::test]
    async fn building_a_served_output_is_already_valid() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![served_row()]])
            .into_connection();
        let (_server, mut client) = connect(session_over(db)).await;

        let results = client
            .build_paths_with_results(&[DerivedPath::Opaque(hello())], BuildMode::Normal)
            .await
            .expect("results");
        assert_eq!(results.len(), 1);
        assert!(matches!(
            &results[0].result.inner,
            BuildResultInner::Success(s) if s.status == SuccessStatus::AlreadyValid
        ));
    }

    #[tokio::test]
    async fn a_missing_opaque_path_fails_the_build() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (_server, mut client) = connect(session_over(db)).await;

        let err = client
            .build_paths(&[DerivedPath::Opaque(hello())], BuildMode::Normal)
            .await
            .expect_err("missing");
        assert!(
            err.to_string().contains("not in the project caches"),
            "{err}"
        );
    }
}
