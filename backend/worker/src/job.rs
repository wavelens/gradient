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
