/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job execution orchestrator.
//!
//! [`JobUpdater`] wraps the WebSocket sender and provides typed methods for
//! reporting progress back to the server during job execution.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_proto::messages::{
    BuildMetrics, BuildOutput, CACHE_QUERY_TIMEOUT, CachedPath, ClientMessage,
    DiscoveredDerivation, EvalCachePullOutcome, EvalCachePushMode, EvalMessageLevel,
    EvalStatsReport, JobUpdateKind, QueryMode,
};
use tokio::sync::oneshot;
use tracing::debug;

use crate::connection::ProtoWriter;
use crate::nix::store::LocalNixStore;
use crate::proto::eval_cache_recv::EvalCacheReceiver;
use crate::proto::nar_recv::NarReceiver;
use gradient_proto::traits::JobReporter;

/// Chunk size for an inline eval-cache push (mirrors the NAR push chunk size).
const EVAL_CACHE_CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// A pending `CacheQuery`: its reply channel plus the owning `job_id` so a
/// finished or aborted job can drop any query it left in flight.
pub(crate) struct CacheWaiter {
    job_id: String,
    reply: oneshot::Sender<Result<Vec<CachedPath>, String>>,
}

/// Shared map from a unique per-query id to its pending `CacheQuery`.
/// Correlating by the query id (not the `job_id`) lets one job keep several
/// CacheQueries in flight - and survive retries that reuse the `job_id` -
/// without their replies colliding: a stale or out-of-order reply reaches the
/// exact waiter that sent it, or none. `Ok` carries a `CacheStatus`,
/// `Err(message)` a server-side `CacheError` (indeterminate - retry, never
/// "inputs absent").
pub(crate) type CacheWaiters = Arc<Mutex<HashMap<String, CacheWaiter>>>;

/// Register a oneshot for `query_id` (scoped to `job_id`) and hand back its
/// receiver.
pub(crate) fn register_cache_waiter(
    waiters: &CacheWaiters,
    query_id: String,
    job_id: String,
) -> oneshot::Receiver<Result<Vec<CachedPath>, String>> {
    let (reply, rx) = oneshot::channel();
    waiters
        .lock()
        .unwrap()
        .insert(query_id, CacheWaiter { job_id, reply });
    rx
}

/// Deliver a `CacheStatus`/`CacheError` to the waiter that sent `query_id`.
/// Returns false (dropping the reply) when no waiter is registered - a late
/// reply for an already-timed-out or superseded query.
pub(crate) fn deliver_cache_reply(
    waiters: &CacheWaiters,
    query_id: &str,
    result: Result<Vec<CachedPath>, String>,
) -> bool {
    match waiters.lock().unwrap().remove(query_id) {
        Some(w) => {
            let _ = w.reply.send(result);
            true
        }
        None => false,
    }
}

/// Drop the waiter for a single timed-out query so a late reply is discarded
/// rather than delivered to a closed channel.
pub(crate) fn forget_cache_waiter(waiters: &CacheWaiters, query_id: &str) {
    waiters.lock().unwrap().remove(query_id);
}

/// Drop every waiter belonging to `job_id` so a query a finished or aborted job
/// left in flight can't leak its slot.
pub(crate) fn forget_cache_waiters_for_job(waiters: &CacheWaiters, job_id: &str) {
    waiters.lock().unwrap().retain(|_, w| w.job_id != job_id);
}

/// Shared map from job-id to a oneshot sender that delivers `KnownDerivations`
/// responses back to the waiting job task.
pub(crate) type KnownDerivationWaiters = Arc<Mutex<HashMap<String, oneshot::Sender<Vec<String>>>>>;

/// Typed sender for reporting job progress back to the server.
///
/// Uses a cloneable [`ProtoWriter`] (mpsc channel) instead of `&mut ProtoConnection`,
/// allowing the job to run in a separate task while the dispatch loop continues
/// to receive messages.
pub struct JobUpdater {
    pub(crate) job_id: String,
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
}

impl JobUpdater {
    pub fn new(
        job_id: String,
        writer: ProtoWriter,
        cache_waiters: CacheWaiters,
        known_derivation_waiters: KnownDerivationWaiters,
        nar_recv: NarReceiver,
        eval_cache_recv: EvalCacheReceiver,
        store: Option<Arc<LocalNixStore>>,
    ) -> Self {
        Self {
            job_id,
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
            eval_cache_recv,
            store,
        }
    }

    /// Pull `fingerprint`'s shared eval-cache blob, if the server has one.
    /// Best-effort: returns `Ok(None)` on miss; `Err` only on transport failure.
    pub async fn pull_eval_cache(&self, fingerprint: &str) -> Result<Option<Vec<u8>>> {
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
                let bytes = crate::http::client()
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
                Ok(Some(bytes))
            }
            EvalCachePullOutcome::Inline { total_bytes, .. } => {
                let bytes = pending.await_inline(total_bytes).await?;
                Ok(Some(bytes))
            }
        }
    }

    /// Push the local eval-cache blob for `fingerprint`. Best-effort.
    pub async fn push_eval_cache(&self, fingerprint: &str, bytes: Vec<u8>) -> Result<()> {
        let size_bytes = bytes.len() as u64;
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
                crate::http::client()
                    .put(&url)
                    .body(bytes)
                    .send()
                    .await
                    .with_context(|| format!("eval-cache PUT {url}"))?
                    .error_for_status()
                    .with_context(|| format!("eval-cache PUT {url} returned non-2xx"))?;
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
                let mut chunks = bytes.chunks(EVAL_CACHE_CHUNK_SIZE).peekable();
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
        cache_query_with_timeout(&self.job_id, &self.writer, &self.cache_waiters, paths, mode).await
    }

    /// Send `NarRequest { paths }` and wait for every requested path to
    /// arrive via chunked `NarPush` frames. Returns the assembled (still
    /// zstd-compressed) NAR bytes per path in the order requested. Each path
    /// has its own [`gradient_proto::messages::TRANSFER_TIMEOUT`].
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
    pub async fn request_nars(&self, paths: Vec<String>) -> Result<Vec<(String, Vec<u8>)>> {
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
        let mut first_err: Option<anyhow::Error> = None;
        for (path, res) in results {
            match res {
                Ok(bytes) => out.push((path, bytes)),
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
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
                update,
            })
            .await
    }
}

/// Send a `QueryKnownDerivations` and wait for the matching `KnownDerivations`,
/// with a hard timeout so a stalled dispatch loop can't hang the eval task.
async fn known_derivations_with_timeout(
    job_id: &str,
    writer: &ProtoWriter,
    waiters: &KnownDerivationWaiters,
    drv_paths: Vec<String>,
) -> Result<Vec<String>> {
    let path_count = drv_paths.len();
    let (tx, rx) = oneshot::channel();
    waiters.lock().unwrap().insert(job_id.to_owned(), tx);
    writer
        .send(ClientMessage::QueryKnownDerivations {
            job_id: job_id.to_owned(),
            drv_paths,
        })
        .await?;
    match tokio::time::timeout(CACHE_QUERY_TIMEOUT, rx).await {
        Ok(Ok(known)) => Ok(known),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "known-derivation waiter dropped - connection closed or superseded?"
        )),
        Err(_) => {
            waiters.lock().unwrap().remove(job_id);
            Err(anyhow::anyhow!(
                "QueryKnownDerivations for {} paths timed out after {}s (job_id={})",
                path_count,
                CACHE_QUERY_TIMEOUT.as_secs(),
                job_id,
            ))
        }
    }
}

/// Send a `CacheQuery` and wait for the matching `CacheStatus`, with a hard
/// timeout so a stalled dispatch loop can't hang the eval task forever.
async fn cache_query_with_timeout(
    job_id: &str,
    writer: &ProtoWriter,
    cache_waiters: &CacheWaiters,
    paths: Vec<String>,
    mode: QueryMode,
) -> Result<Vec<CachedPath>> {
    let path_count = paths.len();
    let query_id = uuid::Uuid::new_v4().to_string();
    let rx = register_cache_waiter(cache_waiters, query_id.clone(), job_id.to_owned());
    writer
        .send(ClientMessage::CacheQuery {
            job_id: job_id.to_owned(),
            query_id: query_id.clone(),
            paths,
            mode,
        })
        .await?;
    match tokio::time::timeout(CACHE_QUERY_TIMEOUT, rx).await {
        // Server could determine cache state: authoritative cached/uncached list.
        Ok(Ok(Ok(cached))) => Ok(cached),
        // Server-side `CacheError`: indeterminate, not "absent". Propagate as a
        // plain error so prefetch classifies it transient (retry) rather than a
        // terminal `InputsUnavailable`.
        Ok(Ok(Err(message))) => Err(anyhow::anyhow!("CacheQuery failed server-side: {message}")),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "cache waiter dropped - connection closed or superseded?"
        )),
        Err(_) => {
            // Drop the waiter so a late reply doesn't deliver to a closed
            // channel and log a spurious warning later.
            forget_cache_waiter(cache_waiters, &query_id);
            Err(anyhow::anyhow!(
                "CacheQuery for {} paths timed out after {}s waiting for reply (job_id={job_id}, query_id={query_id})",
                path_count,
                CACHE_QUERY_TIMEOUT.as_secs(),
            ))
        }
    }
}

#[async_trait]
impl JobReporter for JobUpdater {
    async fn query_cache(
        &mut self,
        paths: Vec<String>,
        mode: QueryMode,
    ) -> Result<Vec<CachedPath>> {
        cache_query_with_timeout(&self.job_id, &self.writer, &self.cache_waiters, paths, mode).await
    }

    async fn query_known_derivations(&mut self, drv_paths: Vec<String>) -> Result<Vec<String>> {
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
        bumped: Vec<gradient_proto::messages::BumpedInputWire>,
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
    use super::*;
    use gradient_test_support::prelude::MockProtoServer;

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

            let conn = crate::connection::ProtoConnection::open(&url)
                .await
                .unwrap();
            let job_id: String = $job_id.to_owned();
            (conn, server_task, job_id)
        }};
    }

    fn make_updater(
        job_id: String,
        conn: crate::connection::ProtoConnection,
    ) -> (JobUpdater, crate::connection::ProtoReader) {
        let (writer, reader) = conn.split();
        let cache_waiters = Arc::new(Mutex::new(HashMap::new()));
        let known_derivation_waiters = Arc::new(Mutex::new(HashMap::new()));
        let nar_recv = NarReceiver::new();
        let eval_cache_recv = EvalCacheReceiver::new();
        let updater = JobUpdater::new(
            job_id,
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
            eval_cache_recv,
            None,
        );
        (updater, reader)
    }

    /// A `CacheQuery` reply is correlated by its unique query id, never the
    /// `job_id`: two queries in flight for the same job must not steal each
    /// other's reply, a stale/unknown reply is dropped, and job cleanup frees a
    /// query left pending.
    #[test]
    fn cache_replies_correlate_by_query_id_not_job_id() {
        use tokio::sync::oneshot::error::TryRecvError;
        let waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
        let mut rx1 = register_cache_waiter(&waiters, "q1".to_string(), "job-A".to_string());
        let mut rx2 = register_cache_waiter(&waiters, "q2".to_string(), "job-A".to_string());

        assert!(deliver_cache_reply(&waiters, "q2", Ok(vec![])));
        assert!(matches!(rx2.try_recv(), Ok(Ok(_))));
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Empty));

        assert!(!deliver_cache_reply(&waiters, "gone", Ok(vec![])));

        forget_cache_waiters_for_job(&waiters, "job-A");
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Closed));
    }

    #[tokio::test]
    async fn updater_report_fetching() {
        let (conn, server_task, job_id) = server_then_client!("job-fetch", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobUpdate { job_id, update } = msg {
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
                error: "something went wrong".to_owned(),
                kind: gradient_proto::messages::BuildFailureKind::Permanent,
                missing_paths: vec![],
            })
            .await
            .unwrap();
        server_task.await.unwrap();
    }
}
