/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::backend::ConnInfo;
use crate::journal::Violation;
use crate::mock::nar::{NarFile, encode};
use crate::mock::spec::Timing;
use crate::mock::store::{MockStore, Origin};
use crate::mock::timing::chunk_delay;
use crate::mock::{MockState, ca_path, ingest, store_path};
use futures::{Stream, StreamExt as _, TryStreamExt as _};
use harmonia_file_nar::archive::NarByteStream;
use harmonia_protocol::daemon::wire::types::Operation;
use harmonia_protocol::daemon::wire::types2::{
    BuildMode, BuildResult, BuildResultInner, BuildResultSuccess, CollectGarbageResponse, GCAction,
    KeyedBuildResult, QueryMissingResult, SuccessStatus,
};
use harmonia_protocol::daemon::{
    AddToStoreItem, ClientOptions, DaemonError, DaemonPath, DaemonResult, DaemonStore,
    FutureResultExt as _, HandshakeDaemonStore, ResultLog, ResultLogExt as _, TrustLevel,
};
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_content_address::ContentAddressMethodAlgorithm;
use harmonia_store_derivation::derivation::BasicDerivation;
use harmonia_store_derivation::derived_path::{DerivedPath, OutputName, SingleDerivedPath};
use harmonia_store_derivation::realisation::{DrvOutput, Realisation, UnkeyedRealisation};
use harmonia_store_path::{StorePath, StorePathHash, StorePathSet};
use harmonia_utils_signature::Signature;
use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::AsyncBufRead;

#[derive(Clone)]
pub struct MockConn {
    pub state: Arc<MockState>,
    pub conn: ConnInfo,
}

fn err(e: impl std::fmt::Display) -> DaemonError {
    DaemonError::custom(e.to_string())
}

fn full(p: &StorePath) -> String {
    format!("/nix/store/{p}")
}

impl MockConn {
    fn unimplemented(&self, op: Operation) -> DaemonError {
        let name = format!("{op:?}");
        self.state
            .journal
            .start(self.conn.id, "unimplemented", vec![name.clone()])
            .finish(false, None);
        self.state
            .journal
            .violation(Violation::Unimplemented { op: name });
        DaemonError::unimplemented(op)
    }

    fn timed<T>(
        &self,
        op: &'static str,
        paths: Vec<String>,
        f: impl FnOnce() -> anyhow::Result<T>,
    ) -> DaemonResult<T> {
        let timer = self.state.journal.start(self.conn.id, op, paths);
        let result = f();
        timer.finish(
            result.is_ok(),
            result.as_ref().err().map(|e| format!("{e:#}")),
        );
        result.map_err(err)
    }

    async fn timed_async<T>(
        &self,
        op: &'static str,
        paths: Vec<String>,
        f: impl Future<Output = anyhow::Result<T>>,
    ) -> DaemonResult<T> {
        let timer = self.state.journal.start(self.conn.id, op, paths);
        let result = f.await;
        timer.finish(
            result.is_ok(),
            result.as_ref().err().map(|e| format!("{e:#}")),
        );
        result.map_err(err)
    }

    fn timing_for(&self, path: &str) -> &Timing {
        self.state
            .config
            .by_output(path)
            .map_or(&self.state.config.timing, |(_, n, _)| &n.timing)
    }

    async fn import<R: AsyncBufRead + Unpin>(
        &self,
        info: &ValidPathInfo,
        source: R,
    ) -> anyhow::Result<()> {
        let key = full(&info.path);
        let nar = ingest::read_with_delays(source, self.timing_for(&key), &key).await?;
        if let Some(ca) = &info.info.ca {
            let name = info.path.name().to_string();
            let computed = ca_path::path_for(&name, ca, &info.info.references)?;
            if computed != info.path {
                self.state
                    .journal
                    .violation(Violation::ContentAddressMismatch {
                        claimed: key,
                        computed: full(&computed),
                    });
                anyhow::bail!("content address does not match {}", info.path);
            }
        }

        self.state
            .store
            .register(&info.path, &nar, info.info.clone(), Origin::Imported)
            .await?;
        Ok(())
    }

    async fn add_ca(
        &self,
        name: &str,
        cam: ContentAddressMethodAlgorithm,
        refs: &StorePathSet,
        source: impl AsyncBufRead + Unpin,
    ) -> anyhow::Result<ValidPathInfo> {
        let dump = ingest::read_with_delays(source, &self.state.config.timing, name).await?;
        let (path, ca) = ca_path::ca_path(name, cam, refs, &dump)?;
        let nar = match cam {
            ContentAddressMethodAlgorithm::NixArchive(_) => dump,
            _ => {
                let file = NarFile {
                    contents: dump,
                    executable: false,
                };
                encode(&BTreeMap::from([(String::new(), file)])).to_vec()
            }
        };
        let origin = if name.ends_with(".drv") {
            Origin::Evaluated
        } else {
            Origin::Added
        };
        let info = MockStore::describe(&nar, refs.clone(), None, Some(ca));
        self.state
            .store
            .register(&path, &nar, info.clone(), origin)
            .await?;
        Ok(ValidPathInfo { path, info })
    }

    async fn dump(&self, path: &StorePath) -> anyhow::Result<Cursor<Vec<u8>>> {
        anyhow::ensure!(self.state.store.is_valid(path)?, "{path} is not valid");
        let chunks: Vec<bytes::Bytes> = NarByteStream::new(self.state.store.real_path(path))
            .try_collect()
            .await?;
        let nar = chunks.concat();
        let key = full(path);
        let timing = self.timing_for(&key);
        let chunk_count = (nar.len() as u64).div_ceil(timing.chunk_bytes.max(1));
        for index in 0..chunk_count {
            tokio::time::sleep(chunk_delay(timing, &key, index)).await;
        }
        Ok(Cursor::new(nar))
    }

    fn drv_outputs(&self, drv: &StorePath) -> Option<Vec<(String, String)>> {
        let (_, node) = self.state.config.by_drv(&full(drv))?;
        Some(
            node.outputs
                .iter()
                .map(|(name, o)| (name.clone(), o.path.clone()))
                .collect(),
        )
    }

    fn outputs_valid(&self, drv: &StorePath) -> anyhow::Result<bool> {
        let Some(outputs) = self.drv_outputs(drv) else {
            return Ok(false);
        };
        for (_, path) in outputs {
            if !self.state.store.is_valid(&store_path(&path)?)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn requested_valid(&self, paths: &[DerivedPath]) -> anyhow::Result<bool> {
        for path in paths {
            let valid = match path {
                DerivedPath::Opaque(p) => self.state.store.is_valid(p)?,
                DerivedPath::Built { drv_path, .. } => match drv_path.as_ref() {
                    SingleDerivedPath::Opaque(drv) => self.outputs_valid(drv)?,
                    SingleDerivedPath::Built { .. } => false,
                },
            };
            if !valid {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn missing(&self, paths: &[DerivedPath]) -> anyhow::Result<QueryMissingResult> {
        let mut result = QueryMissingResult {
            will_build: StorePathSet::new(),
            will_substitute: StorePathSet::new(),
            unknown: StorePathSet::new(),
            download_size: 0,
            nar_size: 0,
        };
        for path in paths {
            match path {
                DerivedPath::Opaque(p) if !self.state.store.is_valid(p)? => {
                    result.unknown.insert(p.clone());
                }
                DerivedPath::Built { drv_path, .. } => {
                    if let SingleDerivedPath::Opaque(drv) = drv_path.as_ref() {
                        if self.drv_outputs(drv).is_none() {
                            result.unknown.insert(drv.clone());
                        } else if !self.outputs_valid(drv)? {
                            result.will_build.insert(drv.clone());
                        }
                    }
                }
                DerivedPath::Opaque(_) => {}
            }
        }
        Ok(result)
    }

    fn output_map(
        &self,
        drv: &StorePath,
    ) -> anyhow::Result<BTreeMap<OutputName, Option<StorePath>>> {
        self.drv_outputs(drv)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, path)| Ok((name.parse()?, Some(store_path(&path)?))))
            .collect()
    }

    fn valid_derivers(&self, path: &StorePath) -> anyhow::Result<StorePathSet> {
        let deriver = self.state.store.info(path)?.and_then(|i| i.deriver);
        let mut derivers = StorePathSet::new();
        if let Some(drv) = deriver
            && self.state.store.is_valid(&drv)?
        {
            derivers.insert(drv);
        }
        Ok(derivers)
    }

    fn journal_only(&self, op: &'static str, paths: Vec<String>) -> DaemonResult<()> {
        self.timed(op, paths, || Ok(()))
    }
}

fn already_valid(path: &DerivedPath) -> KeyedBuildResult {
    KeyedBuildResult {
        path: path.clone(),
        result: BuildResult {
            inner: BuildResultInner::Success(BuildResultSuccess {
                status: SuccessStatus::AlreadyValid,
                built_outputs: BTreeMap::new(),
            }),
            times_built: 0,
            start_time: 0,
            stop_time: 0,
            cpu_user: None,
            cpu_system: None,
        },
    }
}

impl HandshakeDaemonStore for MockConn {
    type Store = Self;

    fn handshake(self) -> impl ResultLog<Output = DaemonResult<Self::Store>> + Send {
        ready(Ok(self)).empty_logs()
    }
}

impl DaemonStore for MockConn {
    fn trust_level(&self) -> Option<TrustLevel> {
        Some(TrustLevel::Trusted)
    }

    fn set_options<'a>(
        &'a mut self,
        _options: &'a ClientOptions,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        ready(self.journal_only("set_options", vec![])).empty_logs()
    }

    fn is_valid_path<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<bool>> + Send + 'a {
        let result = self.timed("is_valid_path", vec![full(path)], || {
            self.state.store.is_valid(path)
        });
        ready(result).empty_logs()
    }

    fn query_valid_paths<'a>(
        &'a mut self,
        paths: &'a StorePathSet,
        _substitute: bool,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + 'a {
        let result = self.timed(
            "query_valid_paths",
            paths.iter().map(full).collect(),
            || {
                let mut valid = StorePathSet::new();
                for path in paths {
                    if self.state.store.is_valid(path)? {
                        valid.insert(path.clone());
                    }
                }
                Ok(valid)
            },
        );
        ready(result).empty_logs()
    }

    fn query_path_info<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<Option<UnkeyedValidPathInfo>>> + Send + 'a {
        let result = self.timed("query_path_info", vec![full(path)], || {
            self.state.store.info(path)
        });
        ready(result).empty_logs()
    }

    fn nar_from_path<'s>(
        &'s mut self,
        path: &'s StorePath,
    ) -> impl ResultLog<Output = DaemonResult<impl AsyncBufRead + Send + use<>>> + Send + 's {
        async move {
            self.timed_async("nar_from_path", vec![full(path)], self.dump(path))
                .await
        }
        .empty_logs()
    }

    fn build_paths<'a>(
        &'a mut self,
        drvs: &'a [DerivedPath],
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        let result = match self.requested_valid(drvs) {
            Ok(true) => self.journal_only("build_paths", vec![]),
            Ok(false) => Err(self.unimplemented(Operation::BuildPaths)),
            Err(e) => Err(err(e)),
        };
        ready(result).empty_logs()
    }

    fn build_paths_with_results<'a>(
        &'a mut self,
        drvs: &'a [DerivedPath],
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<Vec<KeyedBuildResult>>> + Send + 'a {
        let result = match self.requested_valid(drvs) {
            Ok(true) => self
                .journal_only("build_paths_with_results", vec![])
                .map(|()| drvs.iter().map(already_valid).collect()),
            Ok(false) => Err(self.unimplemented(Operation::BuildPathsWithResults)),
            Err(e) => Err(err(e)),
        };
        ready(result).empty_logs()
    }

    fn build_derivation<'a>(
        &'a mut self,
        _drv_path: &'a StorePath,
        _drv: &'a BasicDerivation,
        _mode: BuildMode,
    ) -> impl ResultLog<Output = DaemonResult<BuildResult>> + Send + 'a {
        ready(Err(self.unimplemented(Operation::BuildDerivation))).empty_logs()
    }

    fn query_missing<'a>(
        &'a mut self,
        paths: &'a [DerivedPath],
    ) -> impl ResultLog<Output = DaemonResult<QueryMissingResult>> + Send + 'a {
        ready(self.timed("query_missing", vec![], || self.missing(paths))).empty_logs()
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
        async move {
            self.timed_async(
                "add_to_store_nar",
                vec![full(&info.path)],
                self.import(info, source),
            )
            .await
        }
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
            let timer = self
                .state
                .journal
                .start(self.conn.id, "add_multiple_to_store", vec![]);
            let mut stream = std::pin::pin!(stream);
            let mut result = Ok(());
            while let Some(item) = stream.next().await {
                let imported = match item {
                    Ok(AddToStoreItem { info, reader }) => {
                        let key = full(&info.path);
                        self.timed_async("add_to_store_nar", vec![key], self.import(&info, reader))
                            .await
                    }
                    Err(e) => Err(e),
                };
                if let Err(e) = imported {
                    result = Err(e);
                    break;
                }
            }
            timer.finish(result.is_ok(), result.as_ref().err().map(|e| e.to_string()));
            result
        }
        .empty_logs()
        .boxed_result()
    }

    fn query_all_valid_paths(
        &mut self,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + '_ {
        ready(Err(self.unimplemented(Operation::QueryAllValidPaths))).empty_logs()
    }

    fn query_referrers<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + 'a {
        let result = self.timed("query_referrers", vec![full(path)], || {
            Ok(self.state.store.referrers(path))
        });
        ready(result).empty_logs()
    }

    fn ensure_path<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        let result = self.timed("ensure_path", vec![full(path)], || {
            anyhow::ensure!(
                self.state.store.is_valid(path)?,
                "path {path} is not valid and the mock never substitutes"
            );
            Ok(())
        });
        ready(result).empty_logs()
    }

    fn add_temp_root<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        ready(self.journal_only("add_temp_root", vec![full(path)])).empty_logs()
    }

    fn add_indirect_root<'a>(
        &'a mut self,
        path: &'a DaemonPath,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        let link = String::from_utf8_lossy(path).into_owned();
        ready(self.journal_only("add_indirect_root", vec![link])).empty_logs()
    }

    fn find_roots(
        &mut self,
    ) -> impl ResultLog<Output = DaemonResult<BTreeMap<DaemonPath, StorePath>>> + Send + '_ {
        ready(Err(self.unimplemented(Operation::FindRoots))).empty_logs()
    }

    fn collect_garbage<'a>(
        &'a mut self,
        _action: GCAction,
        _paths_to_delete: &'a StorePathSet,
        _ignore_liveness: bool,
        _max_freed: u64,
    ) -> impl ResultLog<Output = DaemonResult<CollectGarbageResponse>> + Send + 'a {
        ready(Err(self.unimplemented(Operation::CollectGarbage))).empty_logs()
    }

    fn query_path_from_hash_part<'a>(
        &'a mut self,
        hash: &'a StorePathHash,
    ) -> impl ResultLog<Output = DaemonResult<Option<StorePath>>> + Send + 'a {
        let hash = hash.to_string();
        let result = self.timed("query_path_from_hash_part", vec![hash.clone()], || {
            self.state.store.path_from_hash_part(&hash)
        });
        ready(result).empty_logs()
    }

    fn query_substitutable_paths<'a>(
        &'a mut self,
        _paths: &'a StorePathSet,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + 'a {
        let result = self
            .journal_only("query_substitutable_paths", vec![])
            .map(|()| StorePathSet::new());
        ready(result).empty_logs()
    }

    fn query_valid_derivers<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<StorePathSet>> + Send + 'a {
        let result = self.timed("query_valid_derivers", vec![full(path)], || {
            self.valid_derivers(path)
        });
        ready(result).empty_logs()
    }

    fn optimise_store(&mut self) -> impl ResultLog<Output = DaemonResult<()>> + Send + '_ {
        ready(Err(self.unimplemented(Operation::OptimiseStore))).empty_logs()
    }

    fn verify_store(
        &mut self,
        _check_contents: bool,
        _repair: bool,
    ) -> impl ResultLog<Output = DaemonResult<bool>> + Send + '_ {
        ready(Err(self.unimplemented(Operation::VerifyStore))).empty_logs()
    }

    fn add_signatures<'a>(
        &'a mut self,
        _path: &'a StorePath,
        _signatures: &'a [Signature],
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        ready(Err(self.unimplemented(Operation::AddSignatures))).empty_logs()
    }

    fn query_derivation_output_map<'a>(
        &'a mut self,
        path: &'a StorePath,
    ) -> impl ResultLog<Output = DaemonResult<BTreeMap<OutputName, Option<StorePath>>>> + Send + 'a
    {
        let result = self.timed("query_derivation_output_map", vec![full(path)], || {
            self.output_map(path)
        });
        ready(result).empty_logs()
    }

    fn register_drv_output<'a>(
        &'a mut self,
        _realisation: &'a Realisation,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        ready(Err(self.unimplemented(Operation::RegisterDrvOutput))).empty_logs()
    }

    fn query_realisation<'a>(
        &'a mut self,
        _output_id: &'a DrvOutput,
    ) -> impl ResultLog<Output = DaemonResult<Option<UnkeyedRealisation>>> + Send + 'a {
        let result = self
            .journal_only("query_realisation", vec![])
            .map(|()| None);
        ready(result).empty_logs()
    }

    fn submit_output<'a>(
        &'a mut self,
        _path: &'a SingleDerivedPath,
        _output: &'a OutputName,
    ) -> impl ResultLog<Output = DaemonResult<()>> + Send + 'a {
        ready(Err(self.unimplemented(Operation::SubmitOutput))).empty_logs()
    }

    fn add_build_log<'s, 'r, 'p, R>(
        &'s mut self,
        _path: &'p StorePath,
        _source: R,
    ) -> Pin<Box<dyn ResultLog<Output = DaemonResult<()>> + Send + 'r>>
    where
        R: AsyncBufRead + Send + Unpin + 'r,
        's: 'r,
        'p: 'r,
    {
        ready(Err(self.unimplemented(Operation::AddBuildLog)))
            .empty_logs()
            .boxed_result()
    }

    fn add_perm_root<'a>(
        &'a mut self,
        _path: &'a StorePath,
        _gc_root: &'a DaemonPath,
    ) -> impl ResultLog<Output = DaemonResult<DaemonPath>> + Send + 'a {
        ready(Err(self.unimplemented(Operation::AddPermRoot))).empty_logs()
    }

    fn add_ca_to_store<'a, 'r, R>(
        &'a mut self,
        name: &'a str,
        cam: ContentAddressMethodAlgorithm,
        refs: &'a StorePathSet,
        _repair: bool,
        source: R,
    ) -> Pin<Box<dyn ResultLog<Output = DaemonResult<ValidPathInfo>> + Send + 'r>>
    where
        R: AsyncBufRead + Send + Unpin + 'r,
        'a: 'r,
    {
        async move {
            self.timed_async(
                "add_ca_to_store",
                vec![name.to_owned()],
                self.add_ca(name, cam, refs, source),
            )
            .await
        }
        .empty_logs()
        .boxed_result()
    }

    fn add_to_store_scanning<'a, 'r, R>(
        &'a mut self,
        _name: &'a str,
        _cam: ContentAddressMethodAlgorithm,
        _source: R,
    ) -> Pin<Box<dyn ResultLog<Output = DaemonResult<ValidPathInfo>> + Send + 'r>>
    where
        R: AsyncBufRead + Send + Unpin + 'r,
        'a: 'r,
    {
        ready(Err(self.unimplemented(Operation::AddToStoreScanning)))
            .empty_logs()
            .boxed_result()
    }

    fn shutdown(&mut self) -> impl Future<Output = DaemonResult<()>> + Send + '_ {
        ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;
    use crate::mock::spec::DaemonConfig;
    use crate::server::{next_conn, serve_stream};
    use harmonia_store_remote::DaemonClient;
    use std::collections::BTreeSet;

    async fn connect(dir: &tempfile::TempDir) -> (Arc<MockBackend>, impl DaemonStore) {
        let config = DaemonConfig::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("config");
        let backend = MockBackend::new(config, dir.path().to_path_buf(), None)
            .await
            .expect("backend");
        let (client_side, server_side) = tokio::io::duplex(1 << 20);
        serve_stream(&backend, next_conn(None), server_side);
        let (r, w) = tokio::io::split(client_side);
        let client = DaemonClient::builder()
            .connect(r, w)
            .await
            .expect("handshake");
        (backend, client)
    }

    fn item(hash_char: char, name: &str, refs: &[&StorePath]) -> (ValidPathInfo, bytes::Bytes) {
        let file = NarFile {
            contents: name.as_bytes().to_vec(),
            executable: false,
        };
        let nar = encode(&BTreeMap::from([(String::new(), file)]));
        let base = format!("{}-{name}", hash_char.to_string().repeat(32));
        let path = StorePath::from_base_path(&base).expect("path");
        let refs: BTreeSet<StorePath> = refs.iter().map(|r| (*r).clone()).collect();
        let info = MockStore::describe(&nar, refs, None, None);
        (ValidPathInfo { path, info }, nar)
    }

    #[tokio::test]
    async fn add_nar_then_query() {
        let dir = tempfile::tempdir().expect("tmp");
        let (backend, mut client) = connect(&dir).await;
        let (info, nar) = item('1', "a", &[]);
        client
            .add_to_store_nar(&info, &nar[..], false, true)
            .await
            .expect("add");
        assert!(client.is_valid_path(&info.path).await.expect("valid"));
        let got = client
            .query_path_info(&info.path)
            .await
            .expect("info")
            .expect("some");
        assert_eq!(got.nar_hash, info.info.nar_hash);
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn batch_with_internal_references_registers() {
        let dir = tempfile::tempdir().expect("tmp");
        let (backend, mut client) = connect(&dir).await;
        let (dep, dep_nar) = item('1', "dep", &[]);
        let (top, top_nar) = item('2', "top", &[&dep.path]);
        client
            .add_to_store_nar(&dep, &dep_nar[..], false, true)
            .await
            .expect("dep");
        client
            .add_to_store_nar(&top, &top_nar[..], false, true)
            .await
            .expect("top");
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn add_multiple_orders_within_batch() {
        let dir = tempfile::tempdir().expect("tmp");
        let (backend, mut client) = connect(&dir).await;
        let (dep, dep_nar) = item('1', "dep", &[]);
        let (top, top_nar) = item('2', "top", &[&dep.path]);
        let items = futures::stream::iter([
            Ok(AddToStoreItem {
                info: dep.clone(),
                reader: Cursor::new(dep_nar.to_vec()),
            }),
            Ok(AddToStoreItem {
                info: top.clone(),
                reader: Cursor::new(top_nar.to_vec()),
            }),
        ]);
        client
            .add_multiple_to_store(false, true, items)
            .await
            .expect("batch");
        assert!(client.is_valid_path(&top.path).await.expect("valid"));
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn text_ca_add_computes_nix_path() {
        let dir = tempfile::tempdir().expect("tmp");
        let (backend, mut client) = connect(&dir).await;
        let info = client
            .add_ca_to_store(
                "x",
                ContentAddressMethodAlgorithm::Text,
                &StorePathSet::new(),
                false,
                &b"y"[..],
            )
            .await
            .expect("add");
        assert_eq!(info.path.to_string(), "lfngsssysp6h1v4ccqg23c52s9sjl779-x");
        let written = std::fs::read(backend.0.store.real_path(&info.path)).expect("file");
        assert_eq!(written, b"y");
    }

    #[tokio::test]
    async fn unmodeled_op_is_a_recorded_violation() {
        let dir = tempfile::tempdir().expect("tmp");
        let (backend, mut client) = connect(&dir).await;
        assert!(client.optimise_store().await.is_err());
        assert!(matches!(
            backend.0.journal.violations()[0],
            Violation::Unimplemented { .. }
        ));
    }

    #[tokio::test]
    async fn nar_from_path_round_trips() {
        let dir = tempfile::tempdir().expect("tmp");
        let (_, mut client) = connect(&dir).await;
        let (info, nar) = item('1', "a", &[]);
        client
            .add_to_store_nar(&info, &nar[..], false, true)
            .await
            .expect("add");
        let reader = client.nar_from_path(&info.path).await.expect("nar");
        let mut reader = std::pin::pin!(reader);
        let mut read = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut read)
            .await
            .expect("read");
        assert_eq!(read, nar.to_vec());
    }
}
