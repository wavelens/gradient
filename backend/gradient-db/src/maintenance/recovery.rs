/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use gradient_types::*;

fn eval_survives_restart(status: EvaluationStatus) -> bool {
    matches!(status, EvaluationStatus::Queued | EvaluationStatus::Waiting)
}

fn lost_eval_statuses() -> Vec<EvaluationStatus> {
    EvaluationStatus::ACTIVE
        .into_iter()
        .filter(|s| !eval_survives_restart(*s))
        .collect()
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub assignments_closed: u64,
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
    pub builds_unpromoted: u64,
    pub builds_aborted: u64,
    pub evals_aborted: u64,
    pub tasks_forced: u64,
    pub cluster_attempts_closed: u64,
    pub clusters_requeued: u64,
    pub clusters_aborted: u64,
    pub clusters_failed: u64,
}

fn requeue_mid_flight_sql() -> String {
    format!(
        "UPDATE derivation_build SET status = {queued}, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE status = {building} RETURNING derivation",
        queued = crate::sql::status::build(BuildStatus::Queued),
        building = crate::sql::status::build(BuildStatus::Building),
    )
}

crate::sql_fn! {
    REQUEUE_MID_FLIGHT = requeue_mid_flight_sql,
        params = [],
        tier = Sweep;
}

pub async fn recover_interrupted_work<C: ConnectionTrait>(
    conn: &C,
) -> Result<RecoveryReport, DbErr> {
    // Dispatch rows must close before the requeue below.
    // Both dispatch selections would otherwise refuse the requeued work.
    // That refusal would last until each worker is back.
    let mut report = RecoveryReport {
        assignments_closed: crate::scheduling::assignment_record::abandon_all_open_assignments(
            conn,
        )
        .await?,
        ..Default::default()
    };

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

    // The requeue is evaluating no gate, and nothing downstream is re-checking a `Queued` row.
    // It must settle the rows from its own `RETURNING`.
    // Their dependencies can have regressed while the server was down.
    let requeued = crate::graph::promotion::returned_derivations(
        conn.query_all_raw(REQUEUE_MID_FLIGHT.stmt()).await?,
    );
    report.builds_requeued = requeued.len() as u64;
    report.builds_unpromoted = crate::graph::can_start::unpromote_ungated(conn, &requeued)
        .await?
        .len() as u64;
    crate::task_board::dep_counts::bump_graph_version_for_derivations(conn, &requeued).await?;

    let inflight_evals = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(lost_eval_statuses()))
        .all(conn)
        .await?;

    let eval_ids: Vec<EvaluationId> = inflight_evals.iter().map(|e| e.id).collect();
    if !eval_ids.is_empty() {
        let res = EEvaluation::update_many()
            .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Aborted))
            .col_expr(CEvaluation::UpdatedAt, Expr::value(now))
            .col_expr(CEvaluation::FinishedAt, Expr::value(now))
            .col_expr(
                CEvaluation::GraphVersion,
                Expr::col(CEvaluation::GraphVersion).add(1),
            )
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
        .await?;
    }

    if !eval_ids.is_empty() {
        let aborted = abort_shared_builds_for_evals(conn, &eval_ids).await?;
        report.builds_aborted = aborted.len() as u64;
        crate::task_board::dep_counts::bump_graph_version_for_derivations(conn, &aborted).await?;
    }

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

    let clusters = crate::scheduling::cluster::recover_cluster_attempts(conn).await?;
    report.cluster_attempts_closed = clusters.attempts_closed;
    report.clusters_requeued = clusters.clusters_requeued;
    report.clusters_aborted = clusters.clusters_aborted;
    report.clusters_failed = clusters.clusters_failed;

    Ok(report)
}

fn abort_shared_builds_for_evals_sql() -> String {
    format!(
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
        RETURNING db.derivation
        "#,
        aborted = BuildStatus::Aborted as i32,
        created = BuildStatus::Created as i32,
        queued = BuildStatus::Queued as i32,
        building = BuildStatus::Building as i32,
        completed = EvaluationStatus::Completed as i32,
        failed = EvaluationStatus::Failed as i32,
        eval_aborted = EvaluationStatus::Aborted as i32,
    )
}

crate::sql_fn! {
    ABORT_SHARED_BUILDS_FOR_EVALS = abort_shared_builds_for_evals_sql,
        params = [EvaluationIds(64)],
        tier = Sweep;
}

async fn abort_shared_builds_for_evals<C: ConnectionTrait>(
    conn: &C,
    eval_ids: &[EvaluationId],
) -> Result<Vec<DerivationId>, DbErr> {
    let ids: Vec<uuid::Uuid> = eval_ids.iter().map(|e| e.into_inner()).collect();
    let rows = conn
        .query_all_raw(ABORT_SHARED_BUILDS_FOR_EVALS.bind([ids.into()]))
        .await?;

    Ok(crate::graph::promotion::returned_derivations(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn derivation_row(derivation: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([(
            "derivation".to_owned(),
            Value::from(derivation.into_inner()),
        )])
    }

    fn unpromoted_row(derivation: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            (
                "from_status".to_owned(),
                Value::from(crate::sql::status::build(BuildStatus::Queued)),
            ),
            (
                "to_status".to_owned(),
                Value::from(crate::sql::status::build(BuildStatus::Created)),
            ),
        ])
    }

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
        let mid_flight = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 5,
            }])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 3,
            }])
            .append_query_results([vec![
                derivation_row(mid_flight),
                derivation_row(DerivationId::now_v7()),
            ]])
            .append_query_results([vec![unpromoted_row(mid_flight)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![eval_row(EvaluationStatus::Fetching, Some(task_id))]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([crate::test_ctx::inserted_phase_event()])
            .append_query_results([vec![
                derivation_row(DerivationId::now_v7()),
                derivation_row(DerivationId::now_v7()),
                derivation_row(DerivationId::now_v7()),
                derivation_row(DerivationId::now_v7()),
            ]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_exec_results([1, 0, 0, 0, 1].map(|n| MockExecResult {
                last_insert_id: 0,
                rows_affected: n,
            }))
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.assignments_closed, 5);
        assert_eq!(report.attempts_aborted, 3);
        assert_eq!(report.builds_requeued, 2);
        assert_eq!(report.builds_unpromoted, 1);
        assert_eq!(report.builds_aborted, 4);
        assert_eq!(report.evals_aborted, 1);
        assert_eq!(report.tasks_forced, 1);
        assert_eq!(report.cluster_attempts_closed, 1);
        assert_eq!(report.clusters_requeued, 1);
        assert_eq!(report.clusters_aborted, 0);
    }

    #[tokio::test]
    async fn the_assignment_rows_close_before_the_shared_build_requeue() {
        let none = MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([none.clone(), none])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MEval>::new()])
            .append_exec_results([0, 0, 0, 0, 0].map(|n| MockExecResult {
                last_insert_id: 0,
                rows_affected: n,
            }))
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        recover_interrupted_work(&db).await.unwrap();

        let log = db.into_transaction_log();
        let sql: Vec<&str> = log
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| s.sql.as_str())
            .collect();
        let close = sql
            .iter()
            .position(|s| s.starts_with("UPDATE \"dispatched_job\""))
            .expect("recovery closes every open dispatch row");
        let requeue = sql
            .iter()
            .position(|s| s.contains("UPDATE derivation_build SET status"))
            .expect("recovery requeues the mid-flight shared builds");
        assert!(close < requeue, "{sql:?}");
    }

    #[tokio::test]
    async fn clusters_recover_after_the_evaluations_they_hold() {
        let none = MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([none.clone(), none.clone()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MEval>::new()])
            .append_exec_results([none.clone(), none.clone(), none.clone(), none.clone(), none])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        recover_interrupted_work(&db).await.unwrap();

        let log = db.into_transaction_log();
        let sql: Vec<&str> = log
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| s.sql.as_str())
            .collect();
        let evals = sql
            .iter()
            .position(|s| s.contains("FROM \"evaluation\""))
            .expect("4a");
        let clusters = sql
            .iter()
            .position(|s| s.starts_with("UPDATE \"cluster_job\""))
            .expect("step 5");
        assert!(evals < clusters, "{sql:?}");
    }

    #[tokio::test]
    async fn task_force_step_skipped_when_no_pre_build_evals() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MEval>::new()])
            .append_exec_results([0, 0, 0, 0, 0].map(|n| MockExecResult {
                last_insert_id: 0,
                rows_affected: n,
            }))
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.assignments_closed, 0);
        assert_eq!(report.attempts_aborted, 0);
        assert_eq!(report.builds_requeued, 0);
        assert_eq!(report.builds_unpromoted, 0);
        assert_eq!(report.builds_aborted, 0);
        assert_eq!(report.evals_aborted, 0);
        assert_eq!(report.tasks_forced, 0);
    }
}
