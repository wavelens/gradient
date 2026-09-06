/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Detection for evaluations whose terminal job report was lost.
//!
//! An evaluation in [`EvaluationStatus::EVALUATING`] has exactly one exit: the
//! `EvalStreamCompleted` / `EvalFailed` transition the scheduler sends once,
//! when the worker reports the job terminal. Both handlers swallow a job the
//! tracker no longer knows about ("job_completed for unknown job"), and the
//! graph call can time out or lose its mailbox on an actor restart, so that
//! one message is droppable. Nothing else re-drives it: the waiting-state
//! sweep leaves a pre-build eval alone whenever an eval-capable worker is
//! connected, and `recover_interrupted_work` runs only at startup.
//!
//! This query names the survivors: the job's telemetry row is closed, so the
//! worker did report, yet the evaluation never left the evaluating pair.

use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::EvaluationId;
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

use crate::status_sql;

/// An evaluation stranded in the evaluating pair whose newest eval job is
/// already closed, plus the outcome that job reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LostCompletion {
    pub evaluation: EvaluationId,
    pub outcome: DispatchedJobOutcome,
}

/// Evaluations whose newest eval job finished at least `grace_secs` ago while
/// the evaluation itself has not been written since.
///
/// The grace is measured on `evaluation.updated_at`, which any status write
/// refreshes, so an evaluation that is merely slow to be promoted ages out of
/// the result the moment it moves. It must stay above the graph actor's RPC
/// timeout, or a transition still legitimately in flight looks lost.
pub async fn lost_eval_completions<C: ConnectionTrait>(
    db: &C,
    grace_secs: i64,
) -> Result<Vec<LostCompletion>, DbErr> {
    let sql = format!(
        "SELECT ev.id AS evaluation, dj.outcome AS outcome \
         FROM evaluation ev \
         JOIN LATERAL ( \
           SELECT outcome, finished_at FROM dispatched_job \
           WHERE evaluation_id = ev.id AND kind = {eval_kind} \
           ORDER BY dispatched_at DESC LIMIT 1 \
         ) dj ON TRUE \
         WHERE ev.status IN ({evaluating}) \
           AND dj.finished_at IS NOT NULL \
           AND dj.outcome IS NOT NULL \
           AND ev.updated_at < (now() AT TIME ZONE 'UTC') - make_interval(secs => {grace_secs})",
        eval_kind = i16::from(DispatchedJobKind::Eval),
        evaluating = status_sql::eval_in(&EvaluationStatus::EVALUATING),
    );

    let rows = db
        .query_all_raw(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let evaluation = r.try_get::<uuid::Uuid>("", "evaluation").ok()?;
            let outcome =
                DispatchedJobOutcome::try_from(r.try_get::<i16>("", "outcome").ok()?).ok()?;
            Some(LostCompletion {
                evaluation: EvaluationId::new(evaluation),
                outcome,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scope is composed from the pinned enum numbers, never hand-written,
    /// so a renumber cannot silently widen it to `Fetching` or `Building`.
    #[test]
    fn the_scope_is_the_evaluating_pair_only() {
        let scope = status_sql::eval_in(&EvaluationStatus::EVALUATING);
        assert_eq!(scope, "1, 2");
        assert_eq!(i16::from(DispatchedJobKind::Eval), 0);
    }
}
