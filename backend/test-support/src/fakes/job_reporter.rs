/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recording [`JobReporter`] that captures all calls for test assertions.

use anyhow::Result;
use async_trait::async_trait;
use proto::messages::{BuildOutput, DiscoveredDerivation};
use proto::traits::JobReporter;

/// A reported event captured by [`RecordingJobReporter`].
#[derive(Debug, Clone)]
pub enum ReportedEvent {
    Fetching,
    EvaluatingFlake,
    EvaluatingDerivations,
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
    },
    Building {
        build_id: String,
    },
    BuildOutput {
        build_id: String,
        outputs: Vec<BuildOutput>,
    },
    Compressing,
    Signing,
    LogChunk {
        task_index: u32,
        data: Vec<u8>,
    },
}

/// [`JobReporter`] that records every call as a [`ReportedEvent`].
#[derive(Debug, Default)]
pub struct RecordingJobReporter {
    pub events: Vec<ReportedEvent>,
}

impl RecordingJobReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of events recorded.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Get the last `EvalResult` event, if any.
    pub fn last_eval_result(&self) -> Option<&ReportedEvent> {
        self.events
            .iter()
            .rev()
            .find(|e| matches!(e, ReportedEvent::EvalResult { .. }))
    }
}

#[async_trait]
impl JobReporter for RecordingJobReporter {
    async fn report_fetching(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::Fetching);
        Ok(())
    }

    async fn report_evaluating_flake(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::EvaluatingFlake);
        Ok(())
    }

    async fn report_evaluating_derivations(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::EvaluatingDerivations);
        Ok(())
    }

    async fn report_eval_result(
        &mut self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
    ) -> Result<()> {
        self.events.push(ReportedEvent::EvalResult {
            derivations,
            warnings,
        });
        Ok(())
    }

    async fn report_building(&mut self, build_id: String) -> Result<()> {
        self.events.push(ReportedEvent::Building { build_id });
        Ok(())
    }

    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()> {
        self.events
            .push(ReportedEvent::BuildOutput { build_id, outputs });
        Ok(())
    }

    async fn report_compressing(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::Compressing);
        Ok(())
    }

    async fn report_signing(&mut self) -> Result<()> {
        self.events.push(ReportedEvent::Signing);
        Ok(())
    }

    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> {
        self.events
            .push(ReportedEvent::LogChunk { task_index, data });
        Ok(())
    }
}
