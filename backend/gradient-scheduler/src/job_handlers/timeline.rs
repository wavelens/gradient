/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use chrono::NaiveDateTime;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use tracing::{debug, warn};

use gradient_entity::dispatched_job::{DispatchedJobOutcome, Entity as EDispatchedJob};
use gradient_entity::dispatched_job_phase::Model as MDispatchedJobPhase;
use gradient_entity::ids::{DispatchedJobId, DispatchedJobPhaseId};
use gradient_types::*;
use gradient_wire::types::{JobPhase, JobPhaseSpan};

use crate::Scheduler;

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

#[derive(Debug, Clone, PartialEq)]
pub struct ReportedTimeline {
    pub spans: Vec<JobPhaseSpan>,
    pub worker_elapsed_ms: u64,
    pub received_at: NaiveDateTime,
}

impl ReportedTimeline {
    pub fn received(spans: Vec<JobPhaseSpan>, worker_elapsed_ms: u64) -> Self {
        Self {
            spans,
            worker_elapsed_ms,
            received_at: now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineLanding {
    Closed,
    CloseFailed,
    AlreadyClosed,
    NoRow,
    LookupFailed,
}

impl Scheduler {
    pub fn record_job_timeline(
        self: &Arc<Self>,
        assignment_id: DispatchedJobId,
        outcome: DispatchedJobOutcome,
        report: ReportedTimeline,
    ) {
        let scheduler = Arc::clone(self);
        self.state.shutdown.spawn(async move {
            let _ = scheduler
                .persist_job_timeline(assignment_id, outcome, report)
                .await;
        });
    }

    /// A fetch job's cached follow-up is reusing the job key. Its claim would lose to the fetch's
    /// own row while that row is still open.
    pub async fn close_job_timeline(
        &self,
        assignment_id: DispatchedJobId,
        outcome: DispatchedJobOutcome,
        report: ReportedTimeline,
    ) {
        let _ = self
            .persist_job_timeline(assignment_id, outcome, report)
            .await;
    }

    /// An already closed row is routine because several closers can beat a late report. The phase
    /// rows are keyed on the dispatch and always written. The evaluation totals are keyed on the
    /// evaluation and applied only by the report closing the row. A late report for a superseded
    /// dispatch would otherwise overwrite the run that replaced it.
    pub(crate) async fn persist_job_timeline(
        &self,
        assignment_id: DispatchedJobId,
        outcome: DispatchedJobOutcome,
        report: ReportedTimeline,
    ) -> TimelineLanding {
        let row = match EDispatchedJob::find_by_id(assignment_id)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                warn!(dispatch = %assignment_id, ?outcome, "no dispatched_job row for this report; outcome and phase timeline dropped");
                return TimelineLanding::NoRow;
            }
            Err(e) => {
                warn!(dispatch = %assignment_id, error = %e, "dispatched_job lookup for the timeline failed");
                return TimelineLanding::LookupFailed;
            }
        };

        let evaluation_id = row.evaluation_id;
        let landing = if row.finished_at.is_some() {
            debug!(dispatch = %assignment_id, ?outcome, "dispatched_job row was already closed out; keeping the recorded outcome");
            TimelineLanding::AlreadyClosed
        } else {
            let mut active = row.into_active_model();
            active.finished_at = Set(Some(report.received_at));
            active.outcome = Set(Some(outcome));
            active.worker_elapsed_ms = Set(Some(report.worker_elapsed_ms as i64));
            match active.update(&self.state.worker_db).await {
                Ok(_) => TimelineLanding::Closed,
                Err(e) => {
                    warn!(dispatch = %assignment_id, error = %e, "failed to close the dispatched_job row");
                    TimelineLanding::CloseFailed
                }
            }
        };

        let spans = report.spans;
        let rows = phase_rows(assignment_id, &spans);
        if !rows.is_empty()
            && let Err(e) = gradient_entity::dispatched_job_phase::Entity::insert_many(
                rows.into_iter().map(IntoActiveModel::into_active_model),
            )
            .exec(&self.state.worker_db)
            .await
        {
            warn!(dispatch = %assignment_id, error = %e, "failed to insert dispatched_job_phase rows");
        }

        let totals = eval_phase_totals(&spans);
        if landing != TimelineLanding::AlreadyClosed && !totals.is_empty() {
            self.apply_eval_phase_totals(evaluation_id, totals).await;
        }

        landing
    }

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

    #[test]
    fn a_build_only_timeline_has_no_eval_totals() {
        let totals = eval_phase_totals(&[span(JobPhase::Build, 0, 900, None)]);

        assert!(totals.is_empty());
    }
}
