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
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use proto::messages::{
    BuildOutput, CachedPath, ClientMessage, DiscoveredDerivation, JobUpdateKind, QueryMode,
};
use tokio::sync::oneshot;
use tracing::debug;

use crate::connection::ProtoWriter;
use crate::proto::nar_recv::NarReceiver;
use proto::traits::JobReporter;

/// Shared map from job-id to a oneshot sender that delivers `CacheStatus` responses
/// back to the waiting job task.
pub(crate) type CacheWaiters = Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>>;

/// Shared map from job-id to a oneshot sender that delivers `KnownDerivations`
/// responses back to the waiting job task.
pub(crate) type KnownDerivationWaiters =
    Arc<Mutex<HashMap<String, oneshot::Sender<Vec<String>>>>>;

/// Upper bound for how long a single `CacheQuery` may wait for its `CacheStatus`
/// response. The worker dispatch loop processes server messages serially, so a
/// slow `JobOffer` scoring pass can delay routing a `CacheStatus` to the waiter.
/// Without a timeout the eval task would hang forever in pathological cases —
/// surface the stall instead so the eval fails loudly and the operator can act.
const CACHE_QUERY_TIMEOUT: Duration = Duration::from_secs(120);

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
}

impl JobUpdater {
    pub fn new(
        job_id: String,
        writer: ProtoWriter,
        cache_waiters: CacheWaiters,
        known_derivation_waiters: KnownDerivationWaiters,
        nar_recv: NarReceiver,
    ) -> Self {
        Self {
            job_id,
            writer,
            cache_waiters,
            known_derivation_waiters,
            nar_recv,
        }
    }

    pub async fn query_cache(
        &mut self,
        paths: Vec<String>,
        mode: QueryMode,
    ) -> Result<Vec<CachedPath>> {
        cache_query_with_timeout(&self.job_id, &self.writer, &self.cache_waiters, paths, mode).await
    }

    /// Send `NarRequest { paths }` and wait for every requested path to arrive
    /// via chunked `NarPush` frames. Returns the assembled (still
    /// zstd-compressed) NAR bytes per path in the order requested. Each path
    /// has its own [`NAR_RECV_TIMEOUT`].
    pub async fn request_nars(&self, paths: Vec<String>) -> Result<Vec<(String, Vec<u8>)>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // Send the bulk request, then await each path concurrently so a slow
        // one doesn't serialize the rest.
        self.writer.send(ClientMessage::NarRequest {
            job_id: self.job_id.clone(),
            paths: paths.clone(),
        })?;
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            let bytes = self.nar_recv.wait_for(&self.job_id, &p).await?;
            out.push((p, bytes));
        }
        Ok(out)
    }

    pub fn report_fetch_result(&self, flake_source: Option<String>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { flake_source })
    }

    pub fn report_evaluating_flake(&self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake)
    }

    pub fn report_building(&self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id })
    }

    pub fn report_build_output(&self, build_id: String, outputs: Vec<BuildOutput>) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput { build_id, outputs })
    }

    pub fn report_compressing(&self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing)
    }

    /// Forward a chunk of build log output to the server. Sync — returns
    /// immediately after the message is enqueued in the writer's mpsc channel.
    pub fn send_log_chunk(&self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.writer.send(ClientMessage::LogChunk {
            job_id: self.job_id.clone(),
            task_index,
            data,
        })
    }

    fn send_update(&self, update: JobUpdateKind) -> Result<()> {
        debug!(job_id = %self.job_id, ?update, "sending job update");
        self.writer.send(ClientMessage::JobUpdate {
            job_id: self.job_id.clone(),
            update,
        })
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
    writer.send(ClientMessage::QueryKnownDerivations {
        job_id: job_id.to_owned(),
        drv_paths,
    })?;
    match tokio::time::timeout(CACHE_QUERY_TIMEOUT, rx).await {
        Ok(Ok(known)) => Ok(known),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "known-derivation waiter dropped — connection closed or superseded?"
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
    let (tx, rx) = oneshot::channel();
    // Re-using the same key drops any stale sender; the previous waiter then
    // sees `RecvError` and bails out instead of blocking forever.
    cache_waiters.lock().unwrap().insert(job_id.to_owned(), tx);
    writer.send(ClientMessage::CacheQuery {
        job_id: job_id.to_owned(),
        paths,
        mode,
    })?;
    match tokio::time::timeout(CACHE_QUERY_TIMEOUT, rx).await {
        Ok(Ok(cached)) => Ok(cached),
        Ok(Err(_)) => Err(anyhow::anyhow!(
            "cache waiter dropped — connection closed or superseded?"
        )),
        Err(_) => {
            // Drop the waiter so a late CacheStatus doesn't deliver to a
            // closed channel and log a spurious warning later.
            cache_waiters.lock().unwrap().remove(job_id);
            Err(anyhow::anyhow!(
                "CacheQuery for {} paths timed out after {}s waiting for CacheStatus (job_id={})",
                path_count,
                CACHE_QUERY_TIMEOUT.as_secs(),
                job_id,
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
        self.send_update(JobUpdateKind::Fetching)
    }

    async fn report_fetch_result(&mut self, flake_source: Option<String>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { flake_source })
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake)
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingDerivations)
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
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id })
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput { build_id, outputs })
    }

    async fn report_compressing(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing)
    }

    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.writer.send(ClientMessage::LogChunk {
            job_id: self.job_id.clone(),
            task_index,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::prelude::MockProtoServer;

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
        let updater = JobUpdater::new(job_id, writer, cache_waiters, known_derivation_waiters, nar_recv);
        (updater, reader)
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
            .unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_complete() {
        let (conn, server_task, job_id) = server_then_client!("job-done", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobCompleted { job_id } = msg {
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
            .unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_fail() {
        let (conn, server_task, job_id) = server_then_client!("job-fail", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobFailed { job_id, error } = msg {
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
            })
            .unwrap();
        server_task.await.unwrap();
    }
}
