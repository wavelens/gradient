/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::ci_reporter::{CiReport, CiReporter};
use std::sync::Mutex;

/// In-memory `CiReporter` for tests. Records every call in order.
#[derive(Debug, Default)]
pub struct RecordingCiReporter {
    pub calls: Mutex<Vec<CiReport>>,
}

impl RecordingCiReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of all recorded reports in call order.
    pub fn calls(&self) -> Vec<CiReport> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CiReporter for RecordingCiReporter {
    async fn report(&self, report: &CiReport) -> Result<()> {
        self.calls.lock().unwrap().push(report.clone());
        Ok(())
    }
}
