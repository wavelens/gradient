/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::LazyLock;

use crate::score::context::{HistoryPrediction, InstanceContext, ScoredJob, WorkerMetricsView};

static DEFAULT_JOB: LazyLock<ScoredJob<'static>> = LazyLock::new(|| {
    ScoredJob::new_eval(
        "",
        gradient_types::ids::ProjectId::nil(),
        false,
        HistoryPrediction::default(),
    )
});

#[derive(Clone, Copy)]
pub struct JobContext<'a> {
    pub job: &'a ScoredJob<'a>,
    pub missing_count: Option<u32>,
    pub missing_nar_size: Option<u64>,
    pub outputs_present: bool,
    pub substitute_outputs: Option<u32>,
    pub dependency_count: u32,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    pub project_work_share: Option<f32>,
    pub prioritized: bool,
    pub build_request: bool,
    pub ifd: bool,
    pub rescore_count: u32,
    pub now: chrono::NaiveDateTime,
}

impl Default for JobContext<'_> {
    fn default() -> Self {
        let now = gradient_types::now();
        Self {
            job: &DEFAULT_JOB,
            missing_count: None,
            missing_nar_size: None,
            outputs_present: false,
            substitute_outputs: None,
            dependency_count: 0,
            queued_at: now,
            ready_at: now,
            project_work_share: None,
            prioritized: false,
            build_request: false,
            ifd: false,
            rescore_count: 0,
            now,
        }
    }
}

impl JobContext<'_> {
    pub fn ram_need(&self) -> crate::score::RamNeed {
        match self.job.build() {
            Some(b) => crate::score::RamNeed::of_build(
                b.history().predicted_peak_ram_mb,
                self.outputs_present || self.substitute_outputs.is_some(),
                b.is_fetch(),
            ),
            None => crate::score::RamNeed::Negligible,
        }
    }

    pub fn build_history(&self) -> HistoryPrediction {
        if self.outputs_present {
            HistoryPrediction::default()
        } else {
            self.job.history()
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct WorkerContext<'a> {
    pub architectures: &'a [String],
    pub system_features: &'a [String],
    pub fetch: bool,
    pub metrics: Option<WorkerMetricsView>,
}

pub trait ScoreRule: Send + Sync + std::fmt::Debug {
    /// The name is persisted in `dispatched_job.score_breakdown`. It is declared explicitly because
    /// a struct rename must not change the persisted key.
    fn name(&self) -> &'static str;
    fn score(
        &self,
        job: &JobContext<'_>,
        worker: &WorkerContext<'_>,
        instance: &InstanceContext,
    ) -> f64;
    /// A veto is holding the job off this worker for the round, regardless of the summed score.
    /// Penalties dragging the sum under the floor could be out-voted by unrelated bonuses.
    fn veto(
        &self,
        _job: &JobContext<'_>,
        _worker: &WorkerContext<'_>,
        _instance: &InstanceContext,
    ) -> bool {
        false
    }
    fn uses_project_work_share(&self) -> bool {
        false
    }
    fn description(&self) -> &'static str;
}
