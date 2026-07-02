/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::{InstanceContext, ScoredJob, WorkerMetricsView};

/// Everything the policy knows about the candidate job at scoring time.
#[derive(Clone, Copy)]
pub struct JobContext<'a> {
    pub job: &'a ScoredJob<'a>,
    pub missing_count: Option<u32>,
    pub missing_nar_size: Option<u64>,
    pub dependency_count: u32,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    /// Owning org's work share of currently-active builds (0.0..=1.0), computed
    /// by the scheduler at request time only when the policy consumes it.
    pub org_work_share: Option<f32>,
    pub rescore_count: u32,
    /// Scoring-time clock, threaded in so rules are deterministic functions of
    /// their inputs instead of reading the wall clock.
    pub now: chrono::NaiveDateTime,
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
    /// Stable identifier persisted in `dispatched_job.score_breakdown` and the
    /// board's rule catalog - part of the recorded contract, so it is declared
    /// explicitly and never derived from the Rust type name (a struct rename
    /// must not silently change the persisted key).
    fn name(&self) -> &'static str;
    /// Additive contribution to the job's total for this (job, worker) pair.
    /// All magnitudes come from [`crate::weights`].
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64;
    /// Hold sentinel: `true` means this job must not dispatch to this worker
    /// this round, regardless of the summed score. "Don't dispatch yet" is
    /// expressed here explicitly instead of by penalties dragging the sum
    /// under the floor (which unrelated bonuses could silently out-vote).
    fn veto(
        &self,
        _job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> bool {
        false
    }
    /// Whether this rule reads `JobContext::org_work_share`; the scheduler only
    /// computes the share (an O(active builds) pass) when some enabled rule
    /// consumes it.
    fn uses_org_work_share(&self) -> bool {
        false
    }
    /// Human-readable explanation of what the rule rewards or penalizes, surfaced
    /// in the board UI next to the rule name.
    fn description(&self) -> &'static str;
}
