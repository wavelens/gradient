/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use sea_orm::sea_query::Expr;

use gradient_types::*;

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
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
            .filter(CEvaluation::Id.is_in(eval_ids))
            .exec(conn)
            .await?;
        report.evals_aborted = res.rows_affected;
    }

    // 3c. Force re-evaluation of the affected projects.
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
    async fn all_four_operations_populate_report() {
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
            // 3c. force-eval their projects
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.attempts_aborted, 3);
        assert_eq!(report.builds_requeued, 2);
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
            // 3a. SELECT pre-build evals → empty (steps 3b/3c are skipped)
            .append_query_results([Vec::<MEval>::new()])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.attempts_aborted, 0);
        assert_eq!(report.builds_requeued, 0);
        assert_eq!(report.evals_aborted, 0);
        assert_eq!(report.projects_forced, 0);
    }

}
