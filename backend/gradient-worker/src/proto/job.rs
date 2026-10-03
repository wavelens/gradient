/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use gradient_wire::messages::{
    BuildMetrics, BuildOutput, CachedPath, ClientMessage, DiscoveredDerivation,
    EvalCachePullOutcome, EvalMessageLevel, EvalStatsReport, JobPhase, JobUpdateKind, QueryMode,
};
use gradient_wire::session::frame::BULK_CHUNK_SIZE;
use gradient_worker_client::correlation::{
    AssignmentHandle, CacheWaiters, KnownDerivationWaiters, cache_query_with_timeout,
    known_derivations_with_timeout,
};
use tracing::debug;

use crate::executor::timeline::{JobTimeline, PhaseGuard};
use crate::nix::store::LocalNixStore;
use crate::proto::eval_cache_recv::EvalCacheReceiver;
use crate::proto::prefetch::MissingInputs;
use crate::proto::progress::{BuildProgressSink, EvalProgressSender, Progress};
use gradient_wire::traits::{EvalProgressSink, JobReporter};
use gradient_wire::types::{BuildProgressPhase, GrantTarget, UploadMetadata, UploadObject};
use gradient_worker_client::connection::ProtoWriter;
use gradient_worker_client::nar_recv::{NarPayload, NarReceiver, NarUnavailable};
use gradient_worker_client::upload::UploadClient;

pub struct JobUpdater {
    pub(crate) job_id: String,
    pub(crate) assignment_id: AssignmentHandle,
    pub(crate) writer: ProtoWriter,
    pub(crate) cache_waiters: CacheWaiters,
    pub(crate) known_derivation_waiters: KnownDerivationWaiters,
    pub(crate) nar_recv: NarReceiver,
    pub(crate) eval_cache_recv: EvalCacheReceiver,
    pub(crate) store: Option<Arc<LocalNixStore>>,
    pub(crate) timeline: Arc<JobTimeline>,
    pub(crate) uploads: UploadClient,
}

async fn passthrough_blob(
    writer: &ProtoWriter,
    request_id: u64,
    bytes: &[u8],
    resume_offset: u64,
) -> Result<()> {
    let start = (resume_offset as usize).min(bytes.len());
    let mut offset = start as u64;
    for chunk in bytes[start..].chunks(BULK_CHUNK_SIZE) {
        writer
            .send(ClientMessage::UploadChunk {
                request_id,
                data: chunk.to_vec(),
                offset,
                is_final: false,
            })
            .await?;
        offset += chunk.len() as u64;
    }
    writer
        .send(ClientMessage::UploadChunk {
            request_id,
            data: Vec::new(),
            offset,
            is_final: true,
        })
        .await
}

impl JobUpdater {
    #[expect(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn new(
        job_id: String,
        assignment_id: AssignmentHandle,
        writer: ProtoWriter,
        cache_waiters: CacheWaiters,
        known_derivation_waiters: KnownDerivationWaiters,
        nar_recv: NarReceiver,
        eval_cache_recv: EvalCacheReceiver,
        store: Option<Arc<LocalNixStore>>,
        timeline: Arc<JobTimeline>,
        uploads: UploadClient,
    ) -> Self {
        Self {
            job_id,
            assignment_id,
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
            eval_cache_recv,
            store,
            timeline,
            uploads,
        }
    }

    pub fn phase(&self, phase: JobPhase) -> PhaseGuard {
        self.timeline.enter(phase)
    }

    pub async fn pull_eval_cache(&self, fingerprint: &str) -> Result<Option<Vec<u8>>> {
        let mut guard = self.phase(JobPhase::EvalCachePull);
        let mut pending = self.eval_cache_recv.register_pull(&self.job_id);
        self.writer
            .send(ClientMessage::EvalCachePull {
                job_id: self.job_id.clone(),
                fingerprint: fingerprint.to_owned(),
            })
            .await?;

        match pending.await_outcome().await? {
            EvalCachePullOutcome::Miss => Ok(None),
            EvalCachePullOutcome::Presigned { url } => {
                let bytes = gradient_worker_client::http::client()
                    .get(&url)
                    .send()
                    .await
                    .with_context(|| format!("eval-cache GET {url}"))?
                    .error_for_status()
                    .with_context(|| format!("eval-cache GET {url} returned non-2xx"))?
                    .bytes()
                    .await
                    .with_context(|| format!("read eval-cache body of {url}"))?
                    .to_vec();
                guard.record(0, bytes.len() as u64);
                Ok(Some(bytes))
            }
            EvalCachePullOutcome::Inline { total_bytes, .. } => {
                let bytes = pending.await_inline(total_bytes).await?;
                guard.record(0, bytes.len() as u64);
                Ok(Some(bytes))
            }
        }
    }

    pub async fn push_eval_cache(&self, fingerprint: &str, bytes: Vec<u8>) -> Result<()> {
        let size_bytes = bytes.len() as u64;
        let mut guard = self.phase(JobPhase::EvalCachePush);
        guard.record(0, size_bytes);
        let object = UploadObject::EvalCache {
            fingerprint: fingerprint.to_owned(),
        };
        let mut upload = self.uploads.start(&self.job_id, object, size_bytes).await?;
        while let Some((request_id, target)) = upload.next_grant().await? {
            let sent = self
                .send_eval_cache(request_id, &bytes, target)
                .await
                .map(|()| UploadMetadata::EvalCache { size_bytes });
            if upload.settle(sent).await? {
                break;
            }
        }
        Ok(())
    }

    async fn send_eval_cache(
        &self,
        request_id: u64,
        bytes: &[u8],
        target: GrantTarget,
    ) -> Result<()> {
        match target {
            GrantTarget::Passthrough { resume_offset } => {
                passthrough_blob(self.uploads.writer(), request_id, bytes, resume_offset).await
            }
            GrantTarget::Put { url } => {
                gradient_worker_client::object_put::put_object(&url, bytes.to_vec().into(), None)
                    .await
                    .with_context(|| format!("eval-cache PUT {url}"))?;
                Ok(())
            }
            other => bail!("an eval-cache upload cannot use {other:?}"),
        }
    }

    pub async fn query_cache(
        &mut self,
        paths: Vec<String>,
        mode: QueryMode,
    ) -> Result<Vec<CachedPath>> {
        let mut guard = self.phase(JobPhase::CacheQueryWait);
        guard.record(paths.len() as u32, 0);
        cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            paths,
            Vec::new(),
            mode,
            false,
        )
        .await
    }

    pub async fn query_upstream(&mut self, path: String) -> Result<Option<CachedPath>> {
        let mut guard = self.phase(JobPhase::CacheQueryWait);
        guard.record(1, 0);
        let answers = cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            vec![path],
            Vec::new(),
            QueryMode::Pull,
            true,
        )
        .await?;

        Ok(answers.into_iter().find(|cp| cp.cached && cp.url.is_some()))
    }

    pub async fn query_push(
        &self,
        paths: Vec<String>,
        sizes: Vec<Option<u64>>,
    ) -> Result<Vec<CachedPath>> {
        let mut nar_sizes = sizes;
        nar_sizes.resize(paths.len(), None);
        if let Some(store) = &self.store {
            let unknown: Vec<usize> = (0..paths.len())
                .filter(|&i| nar_sizes[i].is_none())
                .collect();
            let unknown_paths: Vec<String> = unknown.iter().map(|&i| paths[i].clone()).collect();
            for (i, size) in unknown
                .into_iter()
                .zip(store.nar_sizes(&unknown_paths).await)
            {
                nar_sizes[i] = size;
            }
        }

        let mut guard = self.phase(JobPhase::CacheQueryWait);
        guard.record(paths.len() as u32, 0);
        cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            paths,
            nar_sizes,
            QueryMode::Push,
            false,
        )
        .await
    }

    pub async fn request_nars(&self, paths: Vec<String>) -> Result<Vec<(String, NarPayload)>> {
        use futures::future::join_all;

        if paths.is_empty() {
            return Ok(Vec::new());
        }

        // Waiters are registered before the request goes on the wire.
        // A server response racing ahead of the next path's await must still find its waiter.
        let pendings: Vec<_> = paths
            .iter()
            .map(|p| self.nar_recv.register(&self.job_id, p))
            .collect();

        let mut fresh = Vec::new();
        for p in &paths {
            match self.nar_recv.resumable(&self.job_id, p).await {
                (received, Some(token)) if received > 0 => {
                    self.writer
                        .send(ClientMessage::NarRequestResume {
                            job_id: self.job_id.clone(),
                            store_path: p.clone(),
                            received_bytes: received,
                            stream_token: token,
                        })
                        .await?;
                }
                _ => fresh.push(p.clone()),
            }
        }
        if !fresh.is_empty() {
            self.writer
                .send(ClientMessage::NarRequest {
                    job_id: self.job_id.clone(),
                    paths: fresh,
                })
                .await?;
        }

        let waits = pendings.into_iter().map(|pending| {
            let recv = self.nar_recv.clone();
            async move {
                let path = pending.store_path().to_owned();
                let res = recv.await_pending(pending).await;
                (path, res)
            }
        });

        let results = join_all(waits).await;
        let mut out = Vec::with_capacity(results.len());
        let mut unavailable = Vec::new();
        let mut first_err: Option<anyhow::Error> = None;
        for (path, res) in results {
            match res {
                Ok(payload) => out.push((path, payload)),
                Err(e) => {
                    if e.downcast_ref::<NarUnavailable>().is_some() {
                        unavailable.push(path);
                    }
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        // A path the cache cannot serve is a missing input, not a transport failure.
        // The server is demoting it and re-queuing its producer.
        // A transient error would only spend another attempt on a NAR no retry can produce.
        if !unavailable.is_empty() {
            return Err(anyhow::Error::new(MissingInputs(unavailable)));
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(out)
    }

    pub async fn report_fetch_result(&self, flake_source: Option<String>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { flake_source })
            .await
    }

    pub async fn report_evaluating_flake(&self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake).await
    }

    pub async fn report_eval_stats(&self, report: EvalStatsReport) -> Result<()> {
        self.send_update(JobUpdateKind::EvalStats(report)).await
    }

    pub async fn report_building(&self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id }).await
    }

    pub async fn report_build_output(
        &self,
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput {
            build_id,
            outputs,
            metrics,
            substituted,
        })
        .await
    }

    pub(crate) fn build_progress(
        &self,
        build_id: String,
        phase: BuildProgressPhase,
    ) -> Progress<BuildProgressSink> {
        Progress::new(BuildProgressSink {
            writer: self.writer.clone(),
            job_id: self.job_id.clone(),
            assignment_id: self.assignment_id.clone(),
            build_id,
            phase,
        })
    }

    pub async fn report_compressing(&self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing).await
    }

    pub async fn send_eval_message(
        &self,
        level: EvalMessageLevel,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<()> {
        self.writer
            .send(ClientMessage::EvalMessage {
                job_id: self.job_id.clone(),
                level,
                source: source.into(),
                message: message.into(),
            })
            .await
    }

    pub async fn send_log_chunk(&self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.writer
            .send(ClientMessage::LogChunk {
                job_id: self.job_id.clone(),
                task_index,
                data,
            })
            .await
    }

    async fn send_update(&self, update: JobUpdateKind) -> Result<()> {
        debug!(job_id = %self.job_id, ?update, "sending job update");
        self.writer
            .send(ClientMessage::JobUpdate {
                job_id: self.job_id.clone(),
                assignment_id: self.assignment_id.get(),
                update,
            })
            .await
    }
}

#[async_trait]
impl JobReporter for JobUpdater {
    fn eval_progress_sink(&self) -> Arc<dyn EvalProgressSink> {
        Arc::new(EvalProgressSender {
            writer: self.writer.clone(),
            job_id: self.job_id.clone(),
            assignment_id: self.assignment_id.clone(),
        })
    }

    async fn query_upstream(&mut self, path: String) -> Result<Option<CachedPath>> {
        JobUpdater::query_upstream(self, path).await
    }

    async fn query_cache(
        &mut self,
        paths: Vec<String>,
        mode: QueryMode,
    ) -> Result<Vec<CachedPath>> {
        cache_query_with_timeout(
            &self.job_id,
            &self.writer,
            &self.cache_waiters,
            paths,
            Vec::new(),
            mode,
            false,
        )
        .await
    }

    #[tracing::instrument(level = "debug", skip_all, fields(job_id = %self.job_id, paths = drv_paths.len()))]
    async fn query_known_derivations(&self, drv_paths: Vec<String>) -> Result<Vec<String>> {
        let mut guard = self.phase(JobPhase::KnownDerivationsWait);
        guard.record(drv_paths.len() as u32, 0);
        known_derivations_with_timeout(
            &self.job_id,
            &self.writer,
            &self.known_derivation_waiters,
            drv_paths,
        )
        .await
    }

    async fn report_fetching(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Fetching).await
    }

    async fn report_fetch_result(&mut self, flake_source: Option<String>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { flake_source })
            .await
    }

    async fn report_input_update(
        &mut self,
        candidate_lock: String,
        bumped: Vec<gradient_wire::messages::BumpedInputWire>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::InputUpdateResult {
            candidate_lock,
            bumped,
        })
        .await
    }

    async fn report_input_expansion(&mut self, matched: Vec<String>) -> Result<()> {
        self.send_update(JobUpdateKind::InputUpdateExpansion { matched })
            .await
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake).await
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingDerivations).await
    }

    #[tracing::instrument(level = "debug", skip_all, fields(job_id = %self.job_id, derivations = derivations.len()))]
    async fn report_eval_result(
        &self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::EvalResult {
            derivations,
            warnings,
            errors,
        })
        .await
    }

    async fn push_paths(&self, paths: &[(String, Option<u64>)]) -> Result<()> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };

        crate::executor::push_paths(paths, self, &store).await
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id }).await
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput {
            build_id,
            outputs,
            metrics,
            substituted,
        })
        .await
    }

    async fn report_compressing(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing).await
    }

    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.writer
            .send(ClientMessage::LogChunk {
                job_id: self.job_id.clone(),
                task_index,
                data,
            })
            .await
    }

    async fn send_eval_message(
        &mut self,
        level: EvalMessageLevel,
        source: &str,
        message: &str,
    ) -> Result<()> {
        self.writer
            .send(ClientMessage::EvalMessage {
                job_id: self.job_id.clone(),
                level,
                source: source.to_owned(),
                message: message.to_owned(),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_test_support::prelude::MockProtoServer;
    use gradient_util::sync::Mutex;
    use gradient_wire::messages::CACHE_QUERY_MAX_PATHS;
    use gradient_worker_client::correlation::{deliver_cache_reply, deliver_known_derivations};
    use std::collections::HashMap;

    macro_rules! server_then_client {
        ($job_id:expr, |$sc:ident| $server_body:expr) => {{
            let server = MockProtoServer::bind().await;
            let url = server.url().to_owned();

            let server_task = tokio::spawn(async move {
                let mut $sc = server.accept().await;
                $server_body
            });

            let conn = gradient_worker_client::connection::ProtoConnection::open(&url)
                .await
                .unwrap();
            let job_id: String = $job_id.to_owned();
            (conn, server_task, job_id)
        }};
    }

    fn make_updater(
        job_id: String,
        conn: gradient_worker_client::connection::ProtoConnection,
    ) -> (JobUpdater, gradient_worker_client::connection::ProtoReader) {
        let (writer, reader, _flush) = conn.split();
        let writer_for_uploads = writer.clone();
        let cache_waiters = Arc::new(Mutex::new(HashMap::new()));
        let known_derivation_waiters = Arc::new(Mutex::new(HashMap::new()));
        let nar_recv = NarReceiver::new();
        let eval_cache_recv = EvalCacheReceiver::new();
        let updater = JobUpdater::new(
            job_id,
            AssignmentHandle::new("dispatch-1".to_owned()),
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
            eval_cache_recv,
            None,
            JobTimeline::new(),
            UploadClient::new(writer_for_uploads, 8),
        );
        (updater, reader)
    }

    fn cached(path: &str) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: true,
            file_size: None,
            nar_size: None,
            url: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    fn pump_replies(
        mut reader: gradient_worker_client::connection::ProtoReader,
        cache_waiters: CacheWaiters,
        known_waiters: KnownDerivationWaiters,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(inbound) = reader.recv().await {
                match inbound {
                    gradient_wire::Inbound::Control(
                        gradient_wire::messages::ServerMessage::CacheStatus { query_id, cached },
                    ) => {
                        deliver_cache_reply(&cache_waiters, &query_id, Ok(cached));
                    }
                    gradient_wire::Inbound::Control(
                        gradient_wire::messages::ServerMessage::KnownDerivations {
                            query_id,
                            known,
                        },
                    ) => {
                        deliver_known_derivations(&known_waiters, &query_id, known);
                    }
                    _ => {}
                }
            }
        })
    }

    #[tokio::test]
    async fn cache_queries_are_pipelined_to_the_window_and_reassembled_in_order() {
        use gradient_wire::messages::{CACHE_QUERY_WINDOW, ServerMessage};
        let chunks = CACHE_QUERY_WINDOW * 2 + 1;
        let total = CACHE_QUERY_MAX_PATHS * (chunks - 1) + 5;
        let (conn, server_task, job_id) = server_then_client!("job-window", |sc| {
            let mut pending: Vec<(String, Vec<String>)> = Vec::new();
            let mut answered = 0usize;
            let mut widest = 0usize;
            while answered < chunks {
                let outstanding = chunks - answered;
                let want = outstanding.min(CACHE_QUERY_WINDOW);
                while pending.len() < want {
                    let msg = sc.recv().await.unwrap();
                    let ClientMessage::CacheQuery {
                        query_id, paths, ..
                    } = msg
                    else {
                        panic!("expected CacheQuery, got {msg:?}");
                    };
                    assert!(paths.len() <= CACHE_QUERY_MAX_PATHS);
                    pending.push((query_id, paths));
                }
                widest = widest.max(pending.len());
                if outstanding > CACHE_QUERY_WINDOW {
                    let extra =
                        tokio::time::timeout(std::time::Duration::from_millis(200), sc.recv())
                            .await;
                    assert!(
                        extra.is_err(),
                        "a query arrived beyond the window: {extra:?}"
                    );
                }
                for (query_id, paths) in pending.drain(..).rev() {
                    let cached = paths.iter().map(|p| cached(p)).collect();
                    sc.send(ServerMessage::CacheStatus { query_id, cached })
                        .await
                        .unwrap();
                    answered += 1;
                }
            }
            widest
        });

        let (mut updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let paths: Vec<String> = (0..total).map(|i| format!("/nix/store/path-{i}")).collect();
        let got = updater
            .query_cache(paths.clone(), QueryMode::Push)
            .await
            .unwrap();

        assert_eq!(got.into_iter().map(|c| c.path).collect::<Vec<_>>(), paths);
        assert_eq!(
            server_task.await.unwrap(),
            CACHE_QUERY_WINDOW,
            "the window must fill"
        );
        pump.abort();
    }

    #[tokio::test]
    async fn a_push_query_without_a_store_marks_every_size_unknown() {
        use gradient_wire::messages::ServerMessage;
        let (conn, server_task, job_id) = server_then_client!("job-sizes", |sc| {
            let msg = sc.recv().await.unwrap();
            let ClientMessage::CacheQuery {
                query_id,
                paths,
                nar_sizes,
                mode,
                ..
            } = msg
            else {
                panic!("expected a CacheQuery");
            };
            assert_eq!(mode, QueryMode::Push);
            assert_eq!(nar_sizes, vec![None; paths.len()]);
            let cached = paths.iter().map(|p| cached(p)).collect();
            sc.send(ServerMessage::CacheStatus { query_id, cached })
                .await
                .unwrap();
        });

        let (updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let paths: Vec<String> = (0..3).map(|i| format!("/nix/store/path-{i}")).collect();
        let got = updater
            .query_push(paths.clone(), vec![None; paths.len()])
            .await
            .unwrap();

        assert_eq!(got.into_iter().map(|c| c.path).collect::<Vec<_>>(), paths);
        server_task.await.unwrap();
        pump.abort();
    }

    #[tokio::test]
    async fn an_unsized_push_query_still_carries_one_size_per_path() {
        use gradient_wire::messages::ServerMessage;
        let (conn, server_task, job_id) = server_then_client!("job-unsized-push", |sc| {
            let msg = sc.recv().await.unwrap();
            let ClientMessage::CacheQuery {
                query_id,
                paths,
                nar_sizes,
                mode,
                ..
            } = msg
            else {
                panic!("expected a CacheQuery");
            };
            assert_eq!(mode, QueryMode::Push);
            assert_eq!(nar_sizes, vec![None; paths.len()]);
            let cached = paths.iter().map(|p| cached(p)).collect();
            sc.send(ServerMessage::CacheStatus { query_id, cached })
                .await
                .unwrap();
        });

        let (mut updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let paths: Vec<String> = (0..2).map(|i| format!("/nix/store/path-{i}")).collect();
        let got = updater
            .query_cache(paths.clone(), QueryMode::Push)
            .await
            .unwrap();

        assert_eq!(got.into_iter().map(|c| c.path).collect::<Vec<_>>(), paths);
        server_task.await.unwrap();
        pump.abort();
    }

    #[tokio::test]
    async fn known_derivation_queries_are_pipelined_and_correlate_by_query_id() {
        use gradient_wire::messages::{CACHE_QUERY_WINDOW, ServerMessage};
        let chunks = CACHE_QUERY_WINDOW * 2 + 1;
        let total = CACHE_QUERY_MAX_PATHS * (chunks - 1) + 5;
        let (conn, server_task, job_id) = server_then_client!("job-known-window", |sc| {
            let mut pending: Vec<(String, Vec<String>)> = Vec::new();
            let mut answered = 0usize;
            let mut widest = 0usize;
            while answered < chunks {
                let outstanding = chunks - answered;
                let want = outstanding.min(CACHE_QUERY_WINDOW);
                while pending.len() < want {
                    let msg = sc.recv().await.unwrap();
                    let ClientMessage::QueryKnownDerivations {
                        query_id,
                        drv_paths,
                        ..
                    } = msg
                    else {
                        panic!("expected QueryKnownDerivations, got {msg:?}");
                    };
                    assert!(drv_paths.len() <= CACHE_QUERY_MAX_PATHS);
                    pending.push((query_id, drv_paths));
                }
                widest = widest.max(pending.len());
                if outstanding > CACHE_QUERY_WINDOW {
                    let extra =
                        tokio::time::timeout(std::time::Duration::from_millis(200), sc.recv())
                            .await;
                    assert!(
                        extra.is_err(),
                        "a query arrived beyond the window: {extra:?}"
                    );
                }
                for (query_id, drv_paths) in pending.drain(..).rev() {
                    sc.send(ServerMessage::KnownDerivations {
                        query_id,
                        known: drv_paths,
                    })
                    .await
                    .unwrap();
                    answered += 1;
                }
            }
            widest
        });

        let (updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let drvs: Vec<String> = (0..total)
            .map(|i| format!("/nix/store/d-{i}.drv"))
            .collect();
        let got = JobReporter::query_known_derivations(&updater, drvs.clone())
            .await
            .unwrap();

        assert_eq!(got, drvs);
        assert_eq!(
            server_task.await.unwrap(),
            CACHE_QUERY_WINDOW,
            "the window must fill"
        );
        pump.abort();
    }
}
