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

use anyhow::Result;
use async_trait::async_trait;
use proto::messages::{BuildOutput, CachedPath, ClientMessage, DiscoveredDerivation, FetchedInput, JobUpdateKind, QueryMode};
use tokio::sync::oneshot;
use tracing::debug;

use crate::connection::ProtoWriter;
use proto::traits::JobReporter;

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
    pub(crate) cache_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>>,
}

impl JobUpdater {
    pub fn new(
        job_id: String,
        writer: ProtoWriter,
        cache_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>>,
    ) -> Self {
        Self { job_id, writer, cache_waiters }
    }

    pub fn job_id(&self) -> &str { &self.job_id }

    pub async fn query_cache(&mut self, paths: Vec<String>, mode: QueryMode) -> Result<Vec<CachedPath>> {
        let (tx, rx) = oneshot::channel();
        self.cache_waiters.lock().unwrap().insert(self.job_id.clone(), tx);
        self.writer.send(ClientMessage::CacheQuery {
            job_id: self.job_id.clone(),
            paths,
            mode,
        })?;
        rx.await.map_err(|_| anyhow::anyhow!("cache waiter dropped — connection closed?"))
    }

    pub fn report_fetching(&self) -> Result<()> {
        self.send_update(JobUpdateKind::Fetching)
    }

    pub fn report_fetch_result(&self, fetched_paths: Vec<FetchedInput>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { fetched_paths })
    }

    pub fn report_evaluating_flake(&self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake)
    }

    pub fn report_evaluating_derivations(&self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingDerivations)
    }

    pub fn report_eval_result(
        &self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::EvalResult { derivations, warnings, errors })
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

    pub fn report_signing(&self) -> Result<()> {
        self.send_update(JobUpdateKind::Signing)
    }

    pub fn send_log_chunk(&self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.writer.send(ClientMessage::LogChunk {
            job_id: self.job_id.clone(),
            task_index,
            data,
        })
    }

    pub fn complete(&self) -> Result<()> {
        self.writer.send(ClientMessage::JobCompleted { job_id: self.job_id.clone() })
    }

    pub fn fail(&self, error: String) -> Result<()> {
        self.writer.send(ClientMessage::JobFailed { job_id: self.job_id.clone(), error })
    }

    fn send_update(&self, update: JobUpdateKind) -> Result<()> {
        debug!(job_id = %self.job_id, ?update, "sending job update");
        self.writer.send(ClientMessage::JobUpdate {
            job_id: self.job_id.clone(),
            update,
        })
    }
}

#[async_trait]
impl JobReporter for JobUpdater {
    async fn query_cache(&mut self, paths: Vec<String>, mode: QueryMode) -> Result<Vec<CachedPath>> {
        let (tx, rx) = oneshot::channel();
        self.cache_waiters.lock().unwrap().insert(self.job_id.clone(), tx);
        self.writer.send(ClientMessage::CacheQuery {
            job_id: self.job_id.clone(),
            paths,
            mode,
        })?;
        rx.await.map_err(|_| anyhow::anyhow!("cache waiter dropped — connection closed?"))
    }

    async fn report_fetching(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Fetching)
    }

    async fn report_fetch_result(&mut self, fetched_paths: Vec<FetchedInput>) -> Result<()> {
        self.send_update(JobUpdateKind::FetchResult { fetched_paths })
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
        self.send_update(JobUpdateKind::EvalResult { derivations, warnings, errors })
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id })
    }

    async fn report_build_output(&mut self, build_id: String, outputs: Vec<BuildOutput>) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput { build_id, outputs })
    }

    async fn report_compressing(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing)
    }

    async fn report_signing(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Signing)
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

    fn make_updater(job_id: String, conn: crate::connection::ProtoConnection) -> (JobUpdater, crate::connection::ProtoReader) {
        let (writer, reader) = conn.split();
        let cache_waiters = Arc::new(Mutex::new(HashMap::new()));
        let updater = JobUpdater::new(job_id, writer, cache_waiters);
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
        updater.report_fetching().unwrap();
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
            .report_eval_result(vec![], vec!["warn1".to_owned()], vec![])
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
            .send_log_chunk(3, b"hello log".to_vec())
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
        updater.complete().unwrap();
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
            .fail("something went wrong".to_owned())
            .unwrap();
        server_task.await.unwrap();
    }
}
