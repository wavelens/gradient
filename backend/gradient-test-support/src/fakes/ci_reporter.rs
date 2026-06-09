/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::ci::{CiReport, CiReporter};
use std::sync::Mutex;

/// A PR/MR comment captured by [`RecordingCiReporter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedComment {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
    pub body: String,
}

/// In-memory `CiReporter` for tests. Records every call in order.
#[derive(Debug, Default)]
pub struct RecordingCiReporter {
    pub calls: Mutex<Vec<CiReport>>,
    pub comments: Mutex<Vec<RecordedComment>>,
}

impl RecordingCiReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of all recorded status reports in call order.
    pub fn calls(&self) -> Vec<CiReport> {
        self.calls.lock().unwrap().clone()
    }

    /// Returns a snapshot of all recorded PR/MR comments in call order.
    pub fn comments(&self) -> Vec<RecordedComment> {
        self.comments.lock().unwrap().clone()
    }
}

#[async_trait]
impl CiReporter for RecordingCiReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        self.calls.lock().unwrap().push(report.clone());
        Ok(None)
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        self.comments.lock().unwrap().push(RecordedComment {
            owner: owner.to_string(),
            repo: repo.to_string(),
            pr_number,
            body: body.to_string(),
        });
        Ok(())
    }
}
