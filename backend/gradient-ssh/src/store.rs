/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::read_log::read_log;
use crate::session::Session;
use crate::store_paths::{StoredPathInfo, original, stored, stored_hash};
use bytes::Bytes;
use futures::{Stream, StreamExt as _};
use gradient_daemon::backend::{Backend, ConnInfo};
use gradient_daemon::journal::Journal;
use gradient_daemon::store_features_beyond_nix_daemon_protocol::StoreFeaturesBeyondDaemonProtocol;
use gradient_db::cache_paths::{ServedPath, served_hashes, served_path};
use gradient_db::store_path_content_variant_reader_choice::highest_ranked_trusted_content_variant_by_store_path_hash_for_project;
use gradient_types::{OriginalStorePath, StorePathMapper};
use gradient_util::nix_hash::normalize_nar_hash;
use harmonia_protocol::daemon::wire::types2::{
    BuildMode, BuildResult, KeyedBuildResult, QueryMissingResult,
};
use harmonia_protocol::daemon::{
    AddToStoreItem, DaemonError, DaemonResult, DaemonStore, FutureResultExt as _,
    HandshakeDaemonStore, ResultLog, ResultLogExt as _, TrustLevel,
};
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_derivation::derivation::BasicDerivation;
use harmonia_store_derivation::derived_path::{DerivedPath, SingleDerivedPath};
use harmonia_store_path::{StoreDir, StorePath, StorePathHash, StorePathSet};
use harmonia_store_path_info::NarHash;
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_signature::Signature;
use std::collections::HashSet;
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

    pub fn session(&self) -> &Session {
        &self.session
    }
}

impl Backend for SshBackend {
    type Handler = CacheStore;

    fn journal(&self) -> &Journal {
        &self.journal
    }

    fn handler(self: &Arc<Self>, _conn: ConnInfo) -> CacheStore {
        new_cache_store_for_session(Arc::clone(&self.session))
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
    mapper: StorePathMapper,
}

pub(crate) fn new_cache_store_for_session(session: Arc<Session>) -> CacheStore {
    CacheStore {
        mapper: session.mapper.clone(),
        session,
    }
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
    DaemonError::custom(format!("{e:#}"))
}

impl CacheStore {
    fn stored_info(&self, info: &ValidPathInfo) -> DaemonResult<StoredPathInfo> {
        StoredPathInfo::inbound(&self.mapper, info).map_err(err)
    }

    async fn served(&self, path: &StorePath) -> DaemonResult<Option<ServedPath>> {
        let path = stored(&self.mapper, path).map_err(err)?;
        self.served_stored(path.hash()).await
    }

    async fn served_stored(&self, hash: &str) -> DaemonResult<Option<ServedPath>> {
        let caches = self
            .session
            .readable_cache_ids_or_closed_error()
            .map_err(err)?;
        let Some(row) = served_path(&self.session.handles.db.web_db, caches, hash)
            .await
            .map_err(err)?
        else {
            return Ok(None);
        };
        if row.package.ends_with(".drv")
            && self
                .store_path_hashes_with_trusted_content_variant(std::slice::from_ref(&row.hash))
                .await?
                .is_empty()
        {
            return Ok(None);
        }
        Ok(Some(row))
    }

    async fn store_path_hashes_with_trusted_content_variant(
        &self,
        hashes: &[String],
    ) -> DaemonResult<HashSet<String>> {
        if hashes.is_empty() {
            return Ok(HashSet::new());
        }
        highest_ranked_trusted_content_variant_by_store_path_hash_for_project(
            &self.session.handles.db.web_db,
            self.session.project.id,
            hashes,
        )
        .await
        .map(|trusted| trusted.into_keys().collect())
        .map_err(err)
    }

    async fn copy_in(
        &self,
        info: &ValidPathInfo,
        reader: impl AsyncBufRead + Send + Unpin,
    ) -> DaemonResult<()> {
        let info = self.stored_info(info)?;
        if self.served_stored(info.path.hash()).await?.is_some() {
            tokio::io::copy(&mut std::pin::pin!(reader), &mut tokio::io::sink()).await?;
            return Ok(());
        }

        crate::ingest::import(&self.session, &info, reader)
            .await
            .map_err(err)
    }

    async fn path_info(&self, row: ServedPath) -> anyhow::Result<UnkeyedValidPathInfo> {
        let handles = &self.session.handles;
        let nar_hash = normalize_nar_hash(&row.nar_hash)
            .parse::<Any<NarHash>>()
            .map_err(|e| anyhow::anyhow!("{}: {e}", row.hash))?
            .into_hash();

        let references = row
            .referenced_store_paths
            .iter()
            .map(|r| original(&self.mapper, r))
            .collect::<anyhow::Result<_>>()?;

        let signatures = row
            .signatures
            .iter()
            .filter_map(|(cache, sig)| {
                gradient_sources::full_signature_token(
                    sig,
                    &handles.db.config.server.serve_url,
                    cache,
                )
                .parse::<Signature>()
                .ok()
            })
            .collect();

        Ok(UnkeyedValidPathInfo {
            deriver: row
                .deriver
                .as_deref()
                .map(|d| original(&self.mapper, d))
                .transpose()?,
            nar_hash,
            references,
            registration_time: None,
            nar_size: row.nar_size as u64,
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
            let stored_paths: Vec<_> = paths
                .iter()
                .map(|p| stored(&self.mapper, p))
                .collect::<anyhow::Result<_>>()
                .map_err(err)?;
            let hashes: Vec<String> = stored_paths.iter().map(|s| s.hash().to_owned()).collect();
            let caches = self
                .session
                .readable_cache_ids_or_closed_error()
                .map_err(err)?;
            let served = served_hashes(&self.session.handles.db.web_db, caches, &hashes)
                .await
                .map_err(err)?;
            let served_derivations: Vec<String> = stored_paths
                .iter()
                .filter(|s| s.is_derivation() && served.contains(s.hash()))
                .map(|s| s.hash().to_owned())
                .collect();
            let trusted = self
                .store_path_hashes_with_trusted_content_variant(&served_derivations)
                .await?;

            Ok(paths
                .iter()
                .zip(&stored_paths)
                .filter(|(_, s)| {
                    served.contains(s.hash()) && (!s.is_derivation() || trusted.contains(s.hash()))
                })
                .map(|(p, _)| p.clone())
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
            let hash = stored_hash(&self.mapper, hash).map_err(err)?;
            let row = self.served_stored(&hash).await?;

            row.map(|r| original(&self.mapper, &format!("{}-{}", r.hash, r.package)).map_err(err))
                .transpose()
        }
        .empty_logs()
    }

    fn nar_from_path<'s>(
        &'s mut self,
        path: &'s StorePath,
    ) -> impl ResultLog<Output = DaemonResult<impl AsyncBufRead + Send + use<>>> + Send + 's {
        async move {
            let stored = stored(&self.mapper, path).map_err(err)?;
            let Some(served) = self.served_stored(stored.hash()).await? else {
                return Err(err(format!("{path} is not in the project caches")));
            };

            crate::nar::open_raw(
                &self.session.handles.db.storage.nar_storage,
                &served.nar_object_storage_key,
            )
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
                let info = self.stored_info(&info)?;
                if self.served_stored(info.path.hash()).await?.is_some() {
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
        crate::daemon_build::build_paths(self.session.clone(), drvs.to_vec())
            .and_then(|_| ready(Ok(())))
    }

    fn build_paths_with_results<'a>(
        &'a mut self,
        drvs: &'a [DerivedPath],
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<Vec<KeyedBuildResult>>> + Send + 'a {
        crate::daemon_build::build_paths(self.session.clone(), drvs.to_vec())
    }

    fn build_derivation<'a>(
        &'a mut self,
        _drv_path: &'a StorePath,
        drv: &'a BasicDerivation,
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<BuildResult>> + Send + 'a {
        crate::daemon_build::build_derivation(self.session.clone(), drv.clone())
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

impl StoreFeaturesBeyondDaemonProtocol for CacheStore {
    async fn read_build_log(&mut self, drv_path: &StorePath) -> DaemonResult<Option<Bytes>> {
        let path = OriginalStorePath::parse(&drv_path.to_string()).map_err(err)?;
        read_log(&self.session, path)
            .await
            .map(|log| log.map(Bytes::from))
            .map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SshHandles;
    use gradient_daemon::server::{next_conn, serve_stream};
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_test_support::state::TestState;
    use gradient_types::CacheId;
    use harmonia_protocol::daemon::wire::types2::{BuildResultInner, SuccessStatus};
    use harmonia_store_path::StorePath;
    use sea_orm::{DatabaseConnection, Value};
    use std::collections::BTreeMap;
    use tokio::task::JoinSet;

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    type TestClient = harmonia_store_remote::DaemonClient<
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    >;

    async fn session_over(db: DatabaseConnection) -> Arc<Session> {
        session_in(TestState::new().web_db(db)).await
    }

    async fn session_in(state: TestState) -> Arc<Session> {
        let session = Session::open(
            SshHandles::for_tests(&state.build()),
            gradient_fixtures::rows::user(),
            gradient_fixtures::rows::project(),
            0,
            vec![CacheId::now_v7()],
            true,
        )
        .await
        .unwrap();
        Arc::new(session)
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
            (
                "nar_object_storage_key".to_owned(),
                Value::from(format!("{HASH}-hello")),
            ),
            ("deriver".to_owned(), Value::from(None::<String>)),
            ("ca".to_owned(), Value::from(None::<String>)),
            ("cache_name".to_owned(), Value::from("main")),
            ("signature".to_owned(), Value::from(vec![7_u8; 64])),
            ("id".to_owned(), Value::from(uuid::Uuid::nil())),
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
        let db = mock_db()
            .append_query_results([vec![served_row()]])
            .into_connection();
        let (_server, mut client) = connect(session_over(db).await).await;

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
    async fn path_info_names_the_path_the_client_asked_for() {
        let (original, stored) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mut row = served_row();
        row.insert("hash".to_owned(), Value::from(stored));
        row.insert(
            "references".to_owned(),
            Value::from(Some(format!("{stored}-hello"))),
        );
        let db = mock_db()
            .append_query_results([vec![row]])
            .into_connection();
        let session = session_in(
            TestState::new()
                .web_db(db.clone())
                .store_path_mapping(Arc::new(RotatedHashMapping)),
        )
        .await;
        let (_server, mut client) = connect(session).await;
        let asked = StorePath::from_base_path(&format!("{original}-hello")).expect("path");

        let info = client
            .query_path_info(&asked)
            .await
            .expect("info")
            .expect("served");
        assert!(info.references.contains(&asked));
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains(stored), "{log}");
        assert!(!log.contains(original), "{log}");
    }

    #[tokio::test]
    async fn view_project_api_key_reads_path_info_from_subscribed_cache() {
        use gradient_types::permissions::{Permission, mask_from};
        use gradient_types::{MProjectAccess, MRole, RoleId};
        let user = gradient_fixtures::rows::user();
        let project = gradient_fixtures::rows::project();
        let role = MRole {
            id: RoleId::now_v7(),
            permission: i64::MAX,
            ..Default::default()
        };
        let access = MProjectAccess {
            project: project.id,
            user: user.id,
            role: role.id,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![user.clone()]])
            .append_query_results([vec![project.clone()]])
            .append_query_results([vec![access]])
            .append_query_results([vec![role]])
            .append_query_results([vec![BTreeMap::from([(
                "cache".to_owned(),
                Value::from(CacheId::now_v7().into_inner()),
            )])]])
            .append_query_results([vec![served_row()]])
            .into_connection();
        let state = TestState::new().web_db(db).build();
        let backend = crate::auth::open_project_store_backend_for_api_key(
            SshHandles::for_tests(&state),
            user.id,
            project.id,
            mask_from(&[Permission::ViewProject]),
            None,
        )
        .await
        .unwrap()
        .expect("store backend opens");
        let (_server, mut client) = connect(Arc::clone(&backend.session)).await;

        let info = client
            .query_path_info(&hello())
            .await
            .expect("info")
            .expect("served");
        assert_eq!(info.nar_size, 42);
    }

    #[tokio::test]
    async fn a_path_outside_the_project_caches_is_invalid() {
        let db = mock_db()
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (_server, mut client) = connect(session_over(db).await).await;

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
    async fn access_recheck_database_error_closes_session_and_next_drv_query_does_not_read_cache_paths_or_reach_drv_trust_check_and_transaction_log_has_only_ssh_key_lookup()
     {
        let db = mock_db()
            .append_query_errors([sea_orm::DbErr::Custom("connection lost".into())])
            .into_connection();
        let session = session_in(TestState::new().web_db(db.clone())).await;

        assert!(
            !crate::server::recheck_session_ssh_key_login_from_database_and_cancel_token_on_change_or_error(
                &session,
                "SHA256:known",
            )
            .await
        );
        assert!(session.closed.is_cancelled());

        let (_server, mut client) = connect(session).await;
        let drv = StorePath::from_base_path(&format!("{HASH}-hello.drv")).expect("path");
        let paths: StorePathSet = [drv.clone()].into_iter().collect();
        let valid = client.query_valid_paths(&paths, false).await;
        assert!(valid.map_or(true, |valid| valid.is_empty()));
        let info = client.query_path_info(&drv).await;
        assert!(info.map_or(true, |info| info.is_none()));

        let log = db.into_transaction_log();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(format!("{log:?}").contains("user_ssh_key"), "{log:?}");
    }

    #[test]
    fn the_client_sees_the_cause_behind_a_context() {
        let e = anyhow::anyhow!("permission denied").context("initiate multipart upload");
        let message = err(e).to_string();
        assert!(message.contains("permission denied"), "{message}");
    }

    #[tokio::test]
    async fn the_client_is_not_trusted() {
        let session = session_over(empty_db()).await;
        let backend = Arc::new(SshBackend::new(session));
        let store = backend.handler(next_conn(None));
        assert_eq!(store.trust_level(), Some(TrustLevel::NotTrusted));
    }

    #[tokio::test]
    async fn garbage_collection_is_refused() {
        let db = empty_db();
        let (_server, mut client) = connect(session_over(db).await).await;

        let err = client.find_roots().await.expect_err("refused");
        assert!(err.to_string().contains("unimplemented"), "{err}");
    }

    #[tokio::test]
    async fn copying_in_a_served_path_imports_nothing() {
        let db = mock_db()
            .append_query_results([vec![served_row()]])
            .into_connection();
        let (_server, mut client) = connect(session_over(db).await).await;
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
        let db = mock_db()
            .append_query_results([vec![served_row()]])
            .into_connection();
        let (_server, mut client) = connect(session_over(db).await).await;

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
    async fn test_session_over_cache_c2_reads_nar_bytes_of_content_variant_served_by_c2() {
        use gradient_fixtures::store_path_content_variants::{
            FIRST_KEY, SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };
        use gradient_test_support::cache_fixture::create_temp_directory_with_one_file_and_get_zstd_compressed_nar;

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let state = TestState::new().web_db(readers.db.connection()).build();
        for (key, contents) in [
            (FIRST_KEY, b"variant of project A"),
            (SECOND_KEY, b"variant of project B"),
        ] {
            let object =
                create_temp_directory_with_one_file_and_get_zstd_compressed_nar("file", contents)
                    .await;
            state.nar_storage.put(key, object).await.unwrap();
        }
        let project = gradient_entity::project::Model {
            id: readers.project_b,
            ..gradient_fixtures::rows::project()
        };
        let session = Session::open(
            SshHandles::for_tests(&state),
            gradient_fixtures::rows::user(),
            project,
            0,
            vec![readers.cache_two],
            true,
        )
        .await
        .unwrap();
        let (_server, mut client) = connect(Arc::new(session)).await;
        let shared = StorePath::from_base_path(&format!("{SHARED}-shared")).expect("path");

        let reader = client.nar_from_path(&shared).await.expect("nar");
        let mut nar = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut std::pin::pin!(reader), &mut nar)
            .await
            .expect("read");

        let nar = String::from_utf8_lossy(&nar);
        assert!(
            nar.contains("variant of project B") && !nar.contains("variant of project A"),
            "Sessions over cache C2 read the bytes of the variant that C2 serves."
        );
        drop(client);
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn a_missing_opaque_path_fails_the_build() {
        let db = mock_db()
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (_server, mut client) = connect(session_over(db).await).await;

        let err = client
            .build_paths(&[DerivedPath::Opaque(hello())], BuildMode::Normal)
            .await
            .expect_err("missing");
        assert!(
            err.to_string().contains("not in the project caches"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn ssh_session_copies_other_project_worker_derivation_when_reading_project_subscription_changes_to_write_only()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::shape::shape;
        use gradient_types::permissions::{Permission, mask_from};
        use gradient_types::{EvaluationId, MProject};
        use sea_orm::{ActiveEnum as _, ConnectionTrait as _};

        const DEP_ATERM: &[u8] = include_bytes!("../testdata/input-addressed-dep.drv");
        const DEP_PATH: &str = include_str!("../testdata/input-addressed-dep.drv.path");

        let base = DEP_PATH.trim().trim_start_matches("/nix/store/");
        let (drv_hash, drv_package) = base.split_once('-').expect("base path");
        let (reading, building) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(reading)
            .into_cache(cache)
            .evaluation(building)
            .into_cache(cache)
            .cached_path(drv_hash, drv_package)
            .into_database()
            .await;
        db.connection()
            .execute_unprepared(&format!(
                "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                 SELECT uuidv7(), id, '{}', '\\x07', 0, now() FROM cached_path",
                cache.into_inner()
            ))
            .await
            .expect("Signature of .drv in Shared Cache");

        let nar_storage_dir =
            std::env::temp_dir().join(format!("gradient-ssh-drv-{}", uuid::Uuid::now_v7()));
        let graph = gradient_graph::Graph::new();
        let state = TestState::new()
            .worker_db(db.connection())
            .cache_db(db.connection())
            .web_db(db.connection())
            .nar_storage(
                gradient_storage::NarStore::local(&nar_storage_dir).expect("local NAR storage"),
            )
            .graph(Arc::clone(&graph))
            .build();
        let _writer = graph
            .spawn(state.db(), None, None)
            .await
            .expect("the graph writer starts");
        let session = Arc::new(
            Session::open(
                SshHandles::for_tests(&state),
                gradient_fixtures::rows::user(),
                MProject {
                    id: db.project_of(reading).await,
                    ..gradient_fixtures::rows::project()
                },
                mask_from(&[Permission::TriggerEvaluation]),
                vec![cache],
                true,
            )
            .await
            .expect("session"),
        );
        db.connection()
            .execute_unprepared(&format!(
                "UPDATE project_cache SET mode = {} WHERE project = '{}'",
                CacheSubscriptionMode::WriteOnly.to_value(),
                db.project_of(reading).await.into_inner()
            ))
            .await
            .expect("The reading project can write to the shared cache but can't read from it while the session is open.");
        let (_server, mut client) = connect(session.clone()).await;
        let drv = StorePath::from_base_path(base).expect("path");

        assert!(
            !client.is_valid_path(&drv).await.expect("valid"),
            "The project doesn't trust any stored variant of the `.drv`. The store can tell the client to copy the variant."
        );

        let nar = nar_of(DEP_ATERM).await;
        client
            .add_to_store_nar(
                &ValidPathInfo {
                    path: drv.clone(),
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
                },
                std::io::Cursor::new(nar),
                false,
                true,
            )
            .await
            .expect("copy the .drv");

        let read = crate::build_request::closure(
            &crate::build_request::VerifiedProjectDrvSource { session: &session },
            &[DEP_PATH.trim().to_owned()],
        )
        .await
        .expect("A copy of the session can create a variant that the project can trust, so the build request can read the .drv.");
        assert_eq!(
            read.read_stored_derivation_path_and_derivation_file_record_pairs
                .len(),
            1
        );
        assert!(client.is_valid_path(&drv).await.expect("valid"));
        db.finish().await;
    }
}
