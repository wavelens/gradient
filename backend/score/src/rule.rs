/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::{ScoredJob, WorkerMetricsView};

/// Everything the policy knows about the candidate job at scoring time.
#[derive(Clone, Copy)]
pub struct JobContext<'a> {
    pub job: &'a ScoredJob<'a>,
    pub missing_count: Option<u32>,
    pub missing_nar_size: Option<u64>,
    pub dependency_count: u32,
    pub queued_at: chrono::NaiveDateTime,
    /// Owning org's fraction of currently-active builds (0.0..=1.0), computed by
    /// the scheduler at request time. `None` when no builds are active.
    pub org_share: Option<f32>,
}

/// Build-relevant capabilities of the requesting worker.
#[derive(Clone, Copy)]
pub struct WorkerContext<'a> {
    pub architectures: &'a [String],
    pub system_features: &'a [String],
    pub fetch: bool,
    pub metrics: Option<WorkerMetricsView>,
}

pub trait ScoreRule: Send + Sync + std::fmt::Debug {
    fn score(&self, job: &JobContext<'_>, worker: &WorkerContext<'_>) -> f64;
}
