/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job execution orchestrator.
//!
//! [`JobUpdater`] wraps the WebSocket sender and provides typed methods for
//! reporting progress back to the server during job execution.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_wire::messages::{
    BuildMetrics, BuildOutput, CachedPath, ClientMessage, DiscoveredDerivation,
    EvalCachePullOutcome, EvalCachePushMode, EvalMessageLevel, EvalStatsReport, JobPhase,
    JobUpdateKind, QueryMode,
};
use gradient_wire::session::frame::BULK_CHUNK_SIZE;
use gradient_worker_client::correlation::{
    CacheWaiters, DispatchHandle, KnownDerivationWaiters, cache_query_with_timeout,
    known_derivations_with_timeout,
};
use tracing::debug;

use crate::executor::timeline::{JobTimeline, PhaseGuard};
use crate::nix::store::LocalNixStore;
use crate::proto::eval_cache_recv::EvalCacheReceiver;
use crate::proto::nar_recv::{NarPayload, NarReceiver, NarUnavailable};
use crate::proto::prefetch::MissingInputs;
use crate::proto::progress::{BuildProgressSink, Progress};
use gradient_wire::traits::JobReporter;
use gradient_worker_client::connection::ProtoWriter;

/// Typed sender for reporting job progress back to the server.
///
/// Uses a cloneable [`ProtoWriter`] (mpsc channel) instead of `&mut ProtoConnection`,
/// allowing the job to run in a separate task while the dispatch loop continues
/// to receive messages.
pub struct JobUpdater {
    pub(crate) job_id: String,
    /// Echoed on every report so the server can drop a stale worker's messages.
    pub(crate) dispatch: DispatchHandle,
    pub(crate) writer: ProtoWriter,
    /// Shared with the dispatch loop: when a `CacheQuery` is sent, a oneshot
    /// sender is registered here; the dispatch loop routes the `CacheStatus`
    /// reply to the waiting job task.
    pub(crate) cache_waiters: CacheWaiters,
    /// Shared with the dispatch loop: when a `QueryKnownDerivations` is sent,
    /// a oneshot sender is registered here; the dispatch loop routes the
    /// `KnownDerivations` reply to the waiting job task.
    pub(crate) known_derivation_waiters: KnownDerivationWaiters,
    /// Routes incoming `NarPush` chunks back to the job task that requested
    /// them via `NarRequest`. Cloneable; cheap.
    pub(crate) nar_recv: NarReceiver,
    /// Routes `EvalCachePullResult` / `EvalCacheChunk` / `EvalCachePushGrant`
    /// back to the job task during the eval-cache pull/push handshake.
    pub(crate) eval_cache_recv: EvalCacheReceiver,
    /// Local store, set for jobs that push NARs (eval closure, build outputs).
    /// `None` in proto round-trip unit tests that never touch the store.
    pub(crate) store: Option<Arc<LocalNixStore>>,
    /// The job's phase timeline. Shared with the dispatch loop so the terminal
    /// message can carry it after the job task is gone.
    pub(crate) timeline: Arc<JobTimeline>,
}

impl JobUpdater {
    #[expect(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn new(
        job_id: String,
        dispatch: DispatchHandle,
        writer: ProtoWriter,
        cache_waiters: CacheWaiters,
        known_derivation_waiters: KnownDerivationWaiters,
        nar_recv: NarReceiver,
        eval_cache_recv: EvalCacheReceiver,
        store: Option<Arc<LocalNixStore>>,
        timeline: Arc<JobTimeline>,
    ) -> Self {
        Self {
            job_id,
            dispatch,
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
            eval_cache_recv,
            store,
            timeline,
        }
    }

    /// Open a phase span on this job's timeline; it closes when the guard drops.
    pub fn phase(&self, phase: JobPhase) -> PhaseGuard {
        self.timeline.enter(phase)
    }

    /// Pull `fingerprint`'s shared eval-cache blob, if the server has one.
    /// Best-effort: returns `Ok(None)` on miss; `Err` only on transport failure.
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

    /// Push the local eval-cache blob for `fingerprint`. Best-effort.
    pub async fn push_eval_cache(&self, fingerprint: &str, bytes: Vec<u8>) -> Result<()> {
        let size_bytes = bytes.len() as u64;
        let mut guard = self.phase(JobPhase::EvalCachePush);
        guard.record(0, size_bytes);
        let mut pending = self.eval_cache_recv.register_push(&self.job_id);
        self.writer
            .send(ClientMessage::EvalCachePush {
                job_id: self.job_id.clone(),
                fingerprint: fingerprint.to_owned(),
                size_bytes,
            })
            .await?;

        match pending.await_grant().await? {
            EvalCachePushMode::Skip => Ok(()),
            EvalCachePushMode::Presigned { url } => {
                super::object_put::put_object(&url, bytes.into(), None)
                    .await
                    .with_context(|| format!("eval-cache PUT {url}"))?;
                self.writer
                    .send(ClientMessage::EvalCachePushDone {
                        job_id: self.job_id.clone(),
                        fingerprint: fingerprint.to_owned(),
                        size_bytes,
                    })
                    .await?;
                Ok(())
            }
            EvalCachePushMode::Inline { .. } => {
                let mut offset: u64 = 0;
                let mut chunks = bytes.chunks(BULK_CHUNK_SIZE).peekable();
                if chunks.peek().is_none() {
                    self.writer
                        .send(ClientMessage::EvalCacheChunk {
                            job_id: self.job_id.clone(),
                            data: Vec::new(),
                            offset: 0,
                            is_final: true,
                        })
                        .await?;
                }

                while let Some(chunk) = chunks.next() {
                    let is_final = chunks.peek().is_none();
                    self.writer
                        .send(ClientMessage::EvalCacheChunk {
                            job_id: self.job_id.clone(),
                            data: chunk.to_vec(),
                            offset,
                            is_final,
                        })
                        .await?;
                    offset += chunk.len() as u64;
                }

                Ok(())
            }
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

    /// One path, and the server may leave our cache for it.
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

    /// `CacheQuery { Push }` with each path's uncompressed size, so the server can
    /// route small NARs over the stream. A size the caller already knows wins; the
    /// rest come from the store, and without a store they stay unknown.
    pub async fn query_push(
        &mut self,
        paths: Vec<String>,
        sizes: Vec<Option<u64>>,
    ) -> Result<Vec<CachedPath>> {
        let from_store = match &self.store {
            Some(store) => store.nar_sizes(&paths).await,
            None => vec![None; paths.len()],
        };
        let nar_sizes: Vec<Option<u64>> = from_store
            .into_iter()
            .enumerate()
            .map(|(i, stored)| sizes.get(i).copied().flatten().or(stored))
            .collect();

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

    /// Send `NarRequest { paths }` and wait for every requested path to
    /// arrive via chunked `NarPush` frames. Returns the assembled (still
    /// zstd-compressed) NAR per path in the order requested, staged on disk
    /// when a partial store is configured. Each path has its own
    /// [`gradient_wire::messages::TRANSFER_TIMEOUT`].
    ///
    /// All waiters are registered **before** the `NarRequest` goes on the
    /// wire so every server response (`NarPush` / `NarUnavailable` /
    /// `NarAbort`) finds a live waiter - otherwise the server's late
    /// responses for paths whose siblings already failed would land in the
    /// dispatch loop with no destination and surface as
    /// "received NarUnavailable/NarAbort with no waiter - discarding"
    /// log spam.
    ///
    /// On the first failure all in-flight waiters are dropped (their
    /// receivers report `RecvError` as the dispatcher discards them) and the
    /// error is returned.
    pub async fn request_nars(&self, paths: Vec<String>) -> Result<Vec<(String, NarPayload)>> {
        use futures::future::join_all;

        if paths.is_empty() {
            return Ok(Vec::new());
        }

        // Register all waiters synchronously before the request goes on the
        // wire so the dispatch loop has somewhere to deliver every server
        // response, even one that races ahead of the next path's await.
        let pendings: Vec<_> = paths
            .iter()
            .map(|p| self.nar_recv.register(&self.job_id, p))
            .collect();

        // Resume any path with a staged `.partial` from a prior interrupted
        // transfer (issue #225); request the rest fresh in one batch. The
        // server self-heals a stale/oversized partial by restarting from 0.
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
        // A path the cache cannot serve is a missing input, not a transport
        // failure: reported as one, the server demotes it and re-queues its
        // producer, where a transient error only spends another attempt against
        // a NAR no retry can produce. The whole batch is reported, so one round
        // trip heals every hole it found.
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

    /// Send the per-eval stats + walked flake-output graph at eval completion.
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

    pub(crate) fn download_progress(&self, build_id: String) -> Progress<BuildProgressSink> {
        Progress::new(BuildProgressSink {
            writer: self.writer.clone(),
            job_id: self.job_id.clone(),
            dispatch: self.dispatch.clone(),
            build_id,
        })
    }

    pub async fn report_compressing(&self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing).await
    }

    /// Report an infrastructure-level message that should surface on the
    /// evaluation page. Use only for transport / prefetch / cache problems -
    /// not for compile failures (those are implicit in `JobFailed`).
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

    /// Forward a chunk of build log output to the server.
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
                dispatch: self.dispatch.get(),
                update,
            })
            .await
    }
}

#[async_trait]
impl JobReporter for JobUpdater {
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

    async fn query_known_derivations(&mut self, drv_paths: Vec<String>) -> Result<Vec<String>> {
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

    async fn report_eval_result(
        &mut self,
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

    async fn push_drv_closure(&mut self, drv_paths: &[String]) -> Result<()> {
        let Some(store) = self.store.clone() else {
            return Ok(());
        };

        crate::executor::push_drv_closure(drv_paths, self, &store).await
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

    /// Spawn the server accept task FIRST (before client opens connection) to
    /// avoid deadlocking on the single-thread tokio test runtime.
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
        let cache_waiters = Arc::new(Mutex::new(HashMap::new()));
        let known_derivation_waiters = Arc::new(Mutex::new(HashMap::new()));
        let nar_recv = NarReceiver::new();
        let eval_cache_recv = EvalCacheReceiver::new();
        let updater = JobUpdater::new(
            job_id,
            DispatchHandle::new("dispatch-1".to_owned()),
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
            eval_cache_recv,
            None,
            JobTimeline::new(),
        );
        (updater, reader)
    }

    #[tokio::test]
    async fn updater_report_fetching() {
        let (conn, server_task, job_id) = server_then_client!("job-fetch", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobUpdate { job_id, update, .. } = msg {
                assert_eq!(job_id, "job-fetch");
                assert!(matches!(update, JobUpdateKind::Fetching));
            } else {
                panic!("expected JobUpdate, got {msg:?}");
            }
        });

        let (updater, _reader) = make_updater(job_id, conn);
        updater
            .writer
            .send(ClientMessage::JobUpdate {
                job_id: updater.job_id.clone(),
                dispatch: updater.dispatch.get(),
                update: JobUpdateKind::Fetching,
            })
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_report_eval_result() {
        let (conn, server_task, job_id) = server_then_client!("job-eval", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobUpdate {
                update:
                    JobUpdateKind::EvalResult {
                        derivations,
                        warnings,
                        errors,
                    },
                ..
            } = msg
            {
                assert_eq!(derivations.len(), 0);
                assert_eq!(warnings, vec!["warn1".to_owned()]);
                assert!(errors.is_empty());
            } else {
                panic!("expected EvalResult, got {msg:?}");
            }
        });

        let (updater, _reader) = make_updater(job_id, conn);
        updater
            .writer
            .send(ClientMessage::JobUpdate {
                job_id: updater.job_id.clone(),
                dispatch: updater.dispatch.get(),
                update: JobUpdateKind::EvalResult {
                    derivations: vec![],
                    warnings: vec!["warn1".to_owned()],
                    errors: vec![],
                },
            })
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_send_log_chunk() {
        let (conn, server_task, job_id) = server_then_client!("job-log", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::LogChunk {
                job_id,
                task_index,
                data,
            } = msg
            {
                assert_eq!(job_id, "job-log");
                assert_eq!(task_index, 3);
                assert_eq!(data, b"hello log".to_vec());
            } else {
                panic!("expected LogChunk, got {msg:?}");
            }
        });

        let (updater, _reader) = make_updater(job_id, conn);
        updater
            .writer
            .send(ClientMessage::LogChunk {
                job_id: updater.job_id.clone(),
                task_index: 3,
                data: b"hello log".to_vec(),
            })
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_complete() {
        let (conn, server_task, job_id) = server_then_client!("job-done", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobCompleted { job_id, .. } = msg {
                assert_eq!(job_id, "job-done");
            } else {
                panic!("expected JobCompleted, got {msg:?}");
            }
        });

        let (updater, _reader) = make_updater(job_id, conn);
        updater
            .writer
            .send(ClientMessage::JobCompleted {
                job_id: updater.job_id.clone(),
                dispatch: updater.dispatch.get(),
                spans: vec![],
            })
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_fail() {
        let (conn, server_task, job_id) = server_then_client!("job-fail", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobFailed { job_id, error, .. } = msg {
                assert_eq!(job_id, "job-fail");
                assert_eq!(error, "something went wrong");
            } else {
                panic!("expected JobFailed, got {msg:?}");
            }
        });

        let (updater, _reader) = make_updater(job_id, conn);
        updater
            .writer
            .send(ClientMessage::JobFailed {
                job_id: updater.job_id.clone(),
                dispatch: updater.dispatch.get(),
                error: "something went wrong".to_owned(),
                kind: gradient_wire::messages::BuildFailureKind::Permanent,
                missing_paths: vec![],
                spans: vec![],
            })
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    fn cached(path: &str) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: true,
            file_size: None,
            nar_size: None,
            url: None,
            multipart: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    /// Stand in for the dispatch loop: route every inbound reply to its waiter.
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

    /// The mock server holds every query until it has a full window, checks
    /// that nothing more arrives while the window is outstanding, then answers
    /// the window in reverse so reassembly order is proven, not assumed.
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

    /// Without a local store no size is known, and an unknown size travels as
    /// `None`, never as a number the server could size an upload by.
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

        let (mut updater, reader) = make_updater(job_id, conn);
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

    /// A caller that cannot know its sizes yet queries through the unsized
    /// `query_cache`. A Push the server accepts still carries one size per path:
    /// unknown, never absent.
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

    /// Same window for the BFS-prune query, and its replies correlate by
    /// `query_id`: answered in reverse, they still merge in request order.
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

        let (mut updater, reader) = make_updater(job_id, conn);
        let pump = pump_replies(
            reader,
            updater.cache_waiters.clone(),
            updater.known_derivation_waiters.clone(),
        );
        let drvs: Vec<String> = (0..total)
            .map(|i| format!("/nix/store/d-{i}.drv"))
            .collect();
        let got = JobReporter::query_known_derivations(&mut updater, drvs.clone())
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
