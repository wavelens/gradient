/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Persists the worker's phase timeline and the job's finish mark.

use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Set,
};
use tracing::warn;

use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobOutcome, Entity as EDispatchedJob,
};
use gradient_entity::dispatched_job_phase::Model as MDispatchedJobPhase;
use gradient_entity::ids::{DispatchedJobId, DispatchedJobPhaseId};
use gradient_types::proto::{JobPhase, JobPhaseSpan};
use gradient_types::*;

use crate::Scheduler;

/// Eval wall-clock per phase, summed across every span of that phase.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvalPhaseTotals {
    pub fetch_ms: i64,
    pub eval_flake_ms: i64,
    pub eval_drv_ms: i64,
}

impl EvalPhaseTotals {
    pub(crate) fn is_empty(&self) -> bool {
        self.fetch_ms == 0 && self.eval_flake_ms == 0 && self.eval_drv_ms == 0
    }
}

pub(crate) fn eval_phase_totals(spans: &[JobPhaseSpan]) -> EvalPhaseTotals {
    let mut totals = EvalPhaseTotals::default();
    for s in spans {
        let ms = s.end_ms.saturating_sub(s.start_ms) as i64;
        match s.phase {
            JobPhase::Fetch => totals.fetch_ms += ms,
            JobPhase::EvalFlake => totals.eval_flake_ms += ms,
            JobPhase::EvalDerivations => totals.eval_drv_ms += ms,
            _ => {}
        }
    }

    totals
}

pub(crate) fn phase_rows(
    dispatched_job: DispatchedJobId,
    spans: &[JobPhaseSpan],
) -> Vec<MDispatchedJobPhase> {
    let created_at = now();
    spans
        .iter()
        .enumerate()
        .map(|(seq, s)| MDispatchedJobPhase {
            id: DispatchedJobPhaseId::now_v7(),
            dispatched_job,
            seq: seq as i32,
            parent_seq: s.parent.map(|p| p as i32),
            phase: s.phase.as_i16(),
            start_ms: s.start_ms as i64,
            end_ms: s.end_ms.max(s.start_ms) as i64,
            paths: s.paths as i32,
            bytes: s.bytes as i64,
            created_at,
        })
        .collect()
}

impl Scheduler {
    /// Close out a job's telemetry: stamp `finished_at` and the outcome, write
    /// one row per span, and fill the eval phase columns the worker no longer
    /// reports directly. Best effort throughout: instrumentation must never
    /// fail a job.
    ///
    /// Only the job lookup runs inline, because the caller is about to drop the
    /// job from the active map; the writes are detached so telemetry never adds
    /// database latency to the connection's message loop.
    pub async fn record_job_timeline(
        self: &Arc<Self>,
        worker_id: &str,
        job_id: &str,
        outcome: DispatchedJobOutcome,
        spans: Vec<JobPhaseSpan>,
    ) {
        let Some(job) = self.active_job(job_id).await else {
            return;
        };
        let evaluation_id = job.evaluation_id();

        let scheduler = Arc::clone(self);
        let worker_id = worker_id.to_owned();
        let job_id = job_id.to_owned();
        self.state.shutdown.spawn(async move {
            scheduler
                .persist_job_timeline(&worker_id, &job_id, evaluation_id, outcome, spans)
                .await;
        });
    }

    async fn persist_job_timeline(
        &self,
        worker_id: &str,
        job_id: &str,
        evaluation_id: EvaluationId,
        outcome: DispatchedJobOutcome,
        spans: Vec<JobPhaseSpan>,
    ) {
        let row = match EDispatchedJob::find()
            .filter(CDispatchedJob::WorkerId.eq(worker_id))
            .filter(CDispatchedJob::EvaluationId.eq(evaluation_id))
            .filter(CDispatchedJob::FinishedAt.is_null())
            .order_by_desc(CDispatchedJob::DispatchedAt)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(e) => {
                warn!(%job_id, error = %e, "dispatched_job lookup for the timeline failed");
                return;
            }
        };

        let dispatched_job = row.id;
        let mut active = row.into_active_model();
        active.finished_at = Set(Some(now()));
        active.outcome = Set(Some(outcome));
        if let Err(e) = active.update(&self.state.worker_db).await {
            warn!(%job_id, error = %e, "failed to close the dispatched_job row");
        }

        let rows = phase_rows(dispatched_job, &spans);
        if !rows.is_empty()
            && let Err(e) = gradient_entity::dispatched_job_phase::Entity::insert_many(
                rows.into_iter().map(IntoActiveModel::into_active_model),
            )
            .exec(&self.state.worker_db)
            .await
        {
            warn!(%job_id, error = %e, "failed to insert dispatched_job_phase rows");
        }

        let totals = eval_phase_totals(&spans);
        if !totals.is_empty() {
            self.apply_eval_phase_totals(evaluation_id, totals).await;
        }
    }

    /// The eval-metric row is written when `EvalStats` arrives, which is before
    /// the timeline; the phase columns are filled in afterwards rather than
    /// held back waiting for it.
    async fn apply_eval_phase_totals(&self, evaluation: EvaluationId, totals: EvalPhaseTotals) {
        use gradient_entity::evaluation_metric::{
            Column as CEvaluationMetric, Entity as EEvaluationMetric,
        };

        if let Err(e) = EEvaluationMetric::update_many()
            .col_expr(CEvaluationMetric::FetchMs, totals.fetch_ms.into())
            .col_expr(CEvaluationMetric::EvalFlakeMs, totals.eval_flake_ms.into())
            .col_expr(CEvaluationMetric::EvalDrvMs, totals.eval_drv_ms.into())
            .filter(CEvaluationMetric::Evaluation.eq(evaluation))
            .exec(&self.state.worker_db)
            .await
        {
            warn!(%evaluation, error = %e, "failed to fill the eval phase columns");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(phase: JobPhase, start_ms: u64, end_ms: u64, parent: Option<u32>) -> JobPhaseSpan {
        JobPhaseSpan {
            phase,
            start_ms,
            end_ms,
            parent,
            ..Default::default()
        }
    }

    /// The wire's positional parent index becomes an explicit `parent_seq`, so
    /// a row can be read back without the original vector.
    #[test]
    fn nesting_becomes_parent_seq() {
        let rows = phase_rows(
            DispatchedJobId::now_v7(),
            &[
                span(JobPhase::Compress, 0, 900, None),
                span(JobPhase::NarPush, 10, 800, Some(0)),
            ],
        );

        assert_eq!(rows[0].seq, 0);
        assert_eq!(rows[0].parent_seq, None);
        assert_eq!(rows[1].seq, 1);
        assert_eq!(rows[1].parent_seq, Some(0));
        assert_eq!(rows[1].phase, JobPhase::NarPush.as_i16());
    }

    /// A span whose end precedes its start would render as a negative bar; it
    /// is clamped rather than dropped, so the phase still appears.
    #[test]
    fn a_backwards_span_is_clamped_not_dropped() {
        let rows = phase_rows(
            DispatchedJobId::now_v7(),
            &[span(JobPhase::Build, 50, 10, None)],
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].start_ms, 50);
        assert_eq!(rows[0].end_ms, 50);
    }

    /// The three eval millisecond columns are the sum of their phases, so an
    /// eval split across several batches still reports one total per phase.
    #[test]
    fn eval_phase_totals_sum_every_matching_span() {
        let spans = [
            span(JobPhase::Fetch, 0, 100, None),
            span(JobPhase::EvalFlake, 100, 250, None),
            span(JobPhase::EvalDerivations, 250, 400, None),
            span(JobPhase::EvalDerivations, 400, 460, None),
        ];

        let totals = eval_phase_totals(&spans);

        assert_eq!(totals.fetch_ms, 100);
        assert_eq!(totals.eval_flake_ms, 150);
        assert_eq!(totals.eval_drv_ms, 210);
    }

    /// A build job contributes no eval totals, so nothing is written.
    #[test]
    fn a_build_only_timeline_has_no_eval_totals() {
        let totals = eval_phase_totals(&[span(JobPhase::Build, 0, 900, None)]);

        assert!(totals.is_empty());
    }
}
