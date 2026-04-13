/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job execution orchestrator.
//!
//! [`JobUpdater`] wraps the WebSocket sender and provides typed methods for
//! reporting progress back to the server during job execution.

use anyhow::Result;
use async_trait::async_trait;
use proto::messages::{
    BuildOutput, ClientMessage, DiscoveredDerivation, JobUpdateKind,
};
use tracing::debug;

use crate::connection::ProtoConnection;
use proto::traits::JobReporter;

/// Typed sender for reporting job progress to the server.
///
/// Every method corresponds to one [`ClientMessage::JobUpdate`] variant.
/// Callers never construct `ClientMessage` directly — they call these methods.
pub struct JobUpdater<'a> {
    pub(crate) job_id: String,
    pub(crate) conn: &'a mut ProtoConnection,
}

impl<'a> JobUpdater<'a> {
    pub fn new(job_id: String, conn: &'a mut ProtoConnection) -> Self {
        Self { job_id, conn }
    }

    pub async fn report_fetching(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Fetching).await
    }

    pub async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingFlake).await
    }

    pub async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::EvaluatingDerivations).await
    }

    pub async fn report_eval_result(
        &mut self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::EvalResult { derivations, warnings })
            .await
    }

    pub async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.send_update(JobUpdateKind::Building { build_id }).await
    }

    pub async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        self.send_update(JobUpdateKind::BuildOutput { build_id, outputs })
            .await
    }

    pub async fn report_compressing(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Compressing).await
    }

    pub async fn report_signing(&mut self) -> Result<()> {
        self.send_update(JobUpdateKind::Signing).await
    }

    pub async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.conn
            .send(ClientMessage::LogChunk {
                job_id: self.job_id.clone(),
                task_index,
                data,
            })
            .await
    }

    pub async fn complete(self) -> Result<()> {
        self.conn
            .send(ClientMessage::JobCompleted { job_id: self.job_id.clone() })
            .await
    }

    pub async fn fail(self, error: String) -> Result<()> {
        self.conn
            .send(ClientMessage::JobFailed {
                job_id: self.job_id.clone(),
                error,
            })
            .await
    }

    async fn send_update(&mut self, update: JobUpdateKind) -> Result<()> {
        debug!(job_id = %self.job_id, ?update, "sending job update");
        self.conn
            .send(ClientMessage::JobUpdate {
                job_id: self.job_id.clone(),
                update,
            })
            .await
    }
}

#[async_trait]
impl JobReporter for JobUpdater<'_> {
    async fn report_fetching(&mut self) -> Result<()> {
        self.report_fetching().await
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.report_evaluating_flake().await
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.report_evaluating_derivations().await
    }

    async fn report_eval_result(
        &mut self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
    ) -> Result<()> {
        self.report_eval_result(derivations, warnings).await
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.report_building(build_id).await
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        self.report_build_output(build_id, outputs).await
    }

    async fn report_compressing(&mut self) -> Result<()> {
        self.report_compressing().await
    }

    async fn report_signing(&mut self) -> Result<()> {
        self.report_signing().await
    }

    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.send_log_chunk(task_index, data).await
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

            let conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
            let job_id: String = $job_id.to_owned();
            (conn, server_task, job_id)
        }};
    }

    #[tokio::test]
    async fn updater_report_fetching() {
        let (mut conn, server_task, job_id) = server_then_client!("job-fetch", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobUpdate { job_id, update } = msg {
                assert_eq!(job_id, "job-fetch");
                assert!(matches!(update, JobUpdateKind::Fetching));
            } else {
                panic!("expected JobUpdate, got {msg:?}");
            }
        });

        let mut updater = JobUpdater::new(job_id, &mut conn);
        updater.report_fetching().await.unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_report_eval_result() {
        let (mut conn, server_task, job_id) = server_then_client!("job-eval", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobUpdate {
                update: JobUpdateKind::EvalResult { derivations, warnings },
                ..
            } = msg
            {
                assert_eq!(derivations.len(), 0);
                assert_eq!(warnings, vec!["warn1".to_owned()]);
            } else {
                panic!("expected EvalResult, got {msg:?}");
            }
        });

        let mut updater = JobUpdater::new(job_id, &mut conn);
        updater.report_eval_result(vec![], vec!["warn1".to_owned()]).await.unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_send_log_chunk() {
        let (mut conn, server_task, job_id) = server_then_client!("job-log", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::LogChunk { job_id, task_index, data } = msg {
                assert_eq!(job_id, "job-log");
                assert_eq!(task_index, 3);
                assert_eq!(data, b"hello log".to_vec());
            } else {
                panic!("expected LogChunk, got {msg:?}");
            }
        });

        let mut updater = JobUpdater::new(job_id, &mut conn);
        updater.send_log_chunk(3, b"hello log".to_vec()).await.unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_complete() {
        let (mut conn, server_task, job_id) = server_then_client!("job-done", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobCompleted { job_id } = msg {
                assert_eq!(job_id, "job-done");
            } else {
                panic!("expected JobCompleted, got {msg:?}");
            }
        });

        let updater = JobUpdater::new(job_id, &mut conn);
        updater.complete().await.unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn updater_fail() {
        let (mut conn, server_task, job_id) = server_then_client!("job-fail", |sc| {
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::JobFailed { job_id, error } = msg {
                assert_eq!(job_id, "job-fail");
                assert_eq!(error, "something went wrong");
            } else {
                panic!("expected JobFailed, got {msg:?}");
            }
        });

        let updater = JobUpdater::new(job_id, &mut conn);
        updater.fail("something went wrong".to_owned()).await.unwrap();
        server_task.await.unwrap();
    }
}
