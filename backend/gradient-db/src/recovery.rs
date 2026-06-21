/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, QueryFilter, Statement};
use sea_orm::sea_query::Expr;

use gradient_types::*;

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
    pub builds_aborted: u64,
    pub evals_aborted: u64,
    pub projects_forced: u64,
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
        .filter(
            gradient_entity::build_attempt::Column::Outcome.eq(AttemptOutcome::Running),
        )
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

    // 3a. Collect pre-build evals that were in-flight on a now-dead worker.
    let pre_build_statuses = [
        EvaluationStatus::Fetching,
        EvaluationStatus::EvaluatingFlake,
        EvaluationStatus::EvaluatingDerivation,
    ];
    let inflight_evals = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(pre_build_statuses))
        .all(conn)
        .await?;

    // 3b. Abort those evaluations.
    let eval_ids: Vec<EvaluationId> = inflight_evals.iter().map(|e| e.id).collect();
    if !eval_ids.is_empty() {
        let res = EEvaluation::update_many()
            .col_expr(
                CEvaluation::Status,
                Expr::value(EvaluationStatus::Aborted),
            )
            .col_expr(CEvaluation::UpdatedAt, Expr::value(now))
            .filter(CEvaluation::Id.is_in(eval_ids.clone()))
            .exec(conn)
            .await?;
        report.evals_aborted = res.rows_affected;
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

    // 3d. Force re-evaluation of the affected projects.
    let project_ids: Vec<ProjectId> = inflight_evals
        .into_iter()
        .filter_map(|e| e.project)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if !project_ids.is_empty() {
        let res = EProject::update_many()
            .col_expr(CProject::ForceEvaluation, Expr::value(true))
            .filter(CProject::Id.is_in(project_ids))
            .exec(conn)
            .await?;
        report.projects_forced = res.rows_affected;
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
        .execute(Statement::from_sql_and_values(
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
    use gradient_entity::evaluation::Model as MEval;
    use gradient_entity::ids::{CommitId, EvaluationId, ProjectId};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn eval_row(status: EvaluationStatus, project: Option<ProjectId>) -> MEval {
        MEval {
            id: EvaluationId::now_v7(),
            project,
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
        let project_id = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. abort orphaned attempts
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 3 }])
            // 2. re-queue Building builds
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 2 }])
            // 3a. SELECT pre-build inflight evals
            .append_query_results([vec![eval_row(EvaluationStatus::Fetching, Some(project_id))]])
            // 3b. abort those evals
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            // 3c. abort their anchors
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 4 }])
            // 3d. force-eval their projects
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.attempts_aborted, 3);
        assert_eq!(report.builds_requeued, 2);
        assert_eq!(report.builds_aborted, 4);
        assert_eq!(report.evals_aborted, 1);
        assert_eq!(report.projects_forced, 1);
    }

    #[tokio::test]
    async fn project_force_step_skipped_when_no_pre_build_evals() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. abort orphaned attempts (none)
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 0 }])
            // 2. re-queue Building builds (none)
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 0 }])
            // 3a. SELECT pre-build evals → empty (steps 3b/3c/3d are skipped)
            .append_query_results([Vec::<MEval>::new()])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.attempts_aborted, 0);
        assert_eq!(report.builds_requeued, 0);
        assert_eq!(report.builds_aborted, 0);
        assert_eq!(report.evals_aborted, 0);
        assert_eq!(report.projects_forced, 0);
    }

}
