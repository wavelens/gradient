/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, QueryFilter, Statement,
};

use gradient_types::*;

/// Evaluations the scheduler re-drives on its own after a restart, so recovery
/// must leave them alone: `Queued` is re-offered by the eval dispatcher and
/// `Waiting` (evaluated, builds queued for a free worker) by build reconcile.
/// Every other active status was running on a now-disconnected worker and is
/// genuinely lost.
fn eval_survives_restart(status: EvaluationStatus) -> bool {
    matches!(status, EvaluationStatus::Queued | EvaluationStatus::Waiting)
}

/// The active statuses this sweep aborts. Derived from `ACTIVE` rather than
/// listed, so a newly added active status is recovered by default instead of
/// silently surviving a restart it cannot survive.
fn lost_eval_statuses() -> Vec<EvaluationStatus> {
    EvaluationStatus::ACTIVE
        .into_iter()
        .filter(|s| !eval_survives_restart(*s))
        .collect()
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
    pub builds_aborted: u64,
    pub evals_aborted: u64,
    pub tasks_forced: u64,
}

pub async fn recover_interrupted_work<C: ConnectionTrait>(
    conn: &C,
) -> Result<RecoveryReport, DbErr> {
    let mut report = RecoveryReport::default();

    // 1. Abort orphaned running attempts.
    let now = now();
    let res = gradient_entity::build_attempt::Entity::update_many()
        .col_expr(
            gradient_entity::build_attempt::Column::Outcome,
            Expr::value(AttemptOutcome::Aborted),
        )
        .col_expr(
            gradient_entity::build_attempt::Column::BuildFinishedAt,
            Expr::value(now),
        )
        .filter(gradient_entity::build_attempt::Column::Outcome.eq(AttemptOutcome::Running))
        .exec(conn)
        .await?;
    report.attempts_aborted = res.rows_affected;

    // 2. Re-queue anchors that were mid-flight (Building → Queued).
    let res = EDerivationBuild::update_many()
        .col_expr(CDerivationBuild::Status, Expr::value(BuildStatus::Queued))
        .col_expr(CDerivationBuild::UpdatedAt, Expr::value(now))
        .filter(CDerivationBuild::Status.eq(BuildStatus::Building))
        .exec(conn)
        .await?;
    report.builds_requeued = res.rows_affected;

    // 3a. Collect the evals a restart lost, `Building` included: their anchors
    // and their terminal transition are this sweep's to finish.
    let inflight_evals = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(lost_eval_statuses()))
        .all(conn)
        .await?;

    // 3b. Abort those evaluations as a complete terminal transition. `finished_at`
    // belongs with the status: a reader that sees Aborted with no end time reads
    // it as still running, and retention keys off the column. The live path
    // (`update_evaluation_status`) also runs the reactor effects; startup has no
    // context for those, so the row is at least consistent on its own.
    let eval_ids: Vec<EvaluationId> = inflight_evals.iter().map(|e| e.id).collect();
    if !eval_ids.is_empty() {
        let res = EEvaluation::update_many()
            .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Aborted))
            .col_expr(CEvaluation::UpdatedAt, Expr::value(now))
            .col_expr(CEvaluation::FinishedAt, Expr::value(now))
            .filter(CEvaluation::Id.is_in(eval_ids.clone()))
            .exec(conn)
            .await?;
        report.evals_aborted = res.rows_affected;

        let subjects: Vec<uuid::Uuid> = eval_ids.iter().map(|e| e.into_inner()).collect();
        crate::status::record_phase_events(
            conn,
            crate::status::PhaseSubjectKind::Evaluation,
            &subjects,
            i32::from(EvaluationStatus::Aborted) as i16,
            now,
        )
        .await;
    }

    // 3c. Abort the anchors those evals drove. When the server dies mid-eval the
    // builder aborts the eval's builds, so reflect it: Created/Queued/Building
    // anchors referenced only by the now-aborted evals go to Aborted. Anchors a
    // still-live eval also needs are left running (shared-anchor safety). The
    // force-eval below re-drives them - `requeue_failed_anchors` resets
    // Aborted -> Created on the next evaluation.
    if !eval_ids.is_empty() {
        report.builds_aborted = abort_anchors_for_evals(conn, &eval_ids).await?;
    }

    // 3d. Force re-evaluation of the affected tasks.
    let task_ids: Vec<TaskId> = inflight_evals
        .into_iter()
        .filter_map(|e| e.task)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if !task_ids.is_empty() {
        let res = ETask::update_many()
            .col_expr(CTask::ForceEvaluation, Expr::value(true))
            .filter(CTask::Id.is_in(task_ids))
            .exec(conn)
            .await?;
        report.tasks_forced = res.rows_affected;
    }

    Ok(report)
}

/// Abort the non-terminal anchors (`Created`/`Queued`/`Building`) driven by
/// `eval_ids`, skipping any a still-live (non-terminal) evaluation also needs.
/// Mirrors the explicit-abort path (`status::abort`): a global build-once anchor
/// is only aborted when no surviving evaluation depends on it. Returns the count.
async fn abort_anchors_for_evals<C: ConnectionTrait>(
    conn: &C,
    eval_ids: &[EvaluationId],
) -> Result<u64, DbErr> {
    let ids: Vec<uuid::Uuid> = eval_ids.iter().map(|e| e.into_inner()).collect();
    let sql = format!(
        r#"
        UPDATE derivation_build db
        SET status = {aborted}, updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.status IN ({created}, {queued}, {building})
          AND EXISTS (
            SELECT 1 FROM build_job bj
            WHERE bj.derivation_build = db.id AND bj.evaluation = ANY($1))
          AND NOT EXISTS (
            SELECT 1 FROM build_job bj2
            JOIN evaluation e2 ON e2.id = bj2.evaluation
            WHERE bj2.derivation_build = db.id
              AND e2.status NOT IN ({completed}, {failed}, {eval_aborted}))
        "#,
        aborted = BuildStatus::Aborted as i32,
        created = BuildStatus::Created as i32,
        queued = BuildStatus::Queued as i32,
        building = BuildStatus::Building as i32,
        completed = EvaluationStatus::Completed as i32,
        failed = EvaluationStatus::Failed as i32,
        eval_aborted = EvaluationStatus::Aborted as i32,
    );

    let res = conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [ids.into()],
        ))
        .await?;

    Ok(res.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Building` is the status this sweep used to miss. Startup aborted such an
    /// evaluation before recovery looked for it, so `abort_anchors_for_evals`
    /// found nothing and every anchor it drove stayed Created/Queued forever.
    #[test]
    fn recovery_owns_every_active_status_a_restart_loses() {
        let lost = lost_eval_statuses();
        assert!(lost.contains(&EvaluationStatus::Building), "{lost:?}");
        assert!(lost.contains(&EvaluationStatus::Fetching), "{lost:?}");
        assert!(
            lost.contains(&EvaluationStatus::EvaluatingFlake),
            "{lost:?}"
        );
        assert!(
            lost.contains(&EvaluationStatus::EvaluatingDerivation),
            "{lost:?}"
        );
    }

    /// The scheduler re-drives these two itself; aborting them would cancel work
    /// that is still live.
    #[test]
    fn recovery_leaves_the_statuses_the_scheduler_re_drives() {
        let lost = lost_eval_statuses();
        assert!(!lost.contains(&EvaluationStatus::Queued), "{lost:?}");
        assert!(!lost.contains(&EvaluationStatus::Waiting), "{lost:?}");
    }

    /// Derived from `ACTIVE`, never listed: a newly added active status must be
    /// recovered by default rather than silently surviving a restart.
    #[test]
    fn the_lost_set_is_exactly_active_minus_the_survivors() {
        let lost = lost_eval_statuses();
        let expected: Vec<EvaluationStatus> = EvaluationStatus::ACTIVE
            .into_iter()
            .filter(|s| !matches!(s, EvaluationStatus::Queued | EvaluationStatus::Waiting))
            .collect();
        assert_eq!(lost, expected);
        assert_eq!(lost.len(), EvaluationStatus::ACTIVE.len() - 2);
    }

    use gradient_entity::evaluation::Model as MEval;
    use gradient_entity::ids::{CommitId, EvaluationId, TaskId};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn eval_row(status: EvaluationStatus, task: Option<TaskId>) -> MEval {
        MEval {
            id: EvaluationId::now_v7(),
            task,
            status,
            repository: "git+https://example.com/repo".into(),
            commit: CommitId::now_v7(),
            wildcard: "**".into(),
            created_at: now(),
            updated_at: now(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn all_operations_populate_report() {
        let task_id = TaskId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. abort orphaned attempts
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 3,
            }])
            // 2. re-queue Building builds
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            // 3a. SELECT pre-build inflight evals
            .append_query_results([vec![eval_row(EvaluationStatus::Fetching, Some(task_id))]])
            // 3b. abort those evals
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // 3c. abort their anchors
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 4,
            }])
            // 3d. force-eval their tasks
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.attempts_aborted, 3);
        assert_eq!(report.builds_requeued, 2);
        assert_eq!(report.builds_aborted, 4);
        assert_eq!(report.evals_aborted, 1);
        assert_eq!(report.tasks_forced, 1);
    }

    #[tokio::test]
    async fn task_force_step_skipped_when_no_pre_build_evals() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. abort orphaned attempts (none)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            // 2. re-queue Building builds (none)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            // 3a. SELECT pre-build evals → empty (steps 3b/3c/3d are skipped)
            .append_query_results([Vec::<MEval>::new()])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.attempts_aborted, 0);
        assert_eq!(report.builds_requeued, 0);
        assert_eq!(report.builds_aborted, 0);
        assert_eq!(report.evals_aborted, 0);
        assert_eq!(report.tasks_forced, 0);
    }
}
