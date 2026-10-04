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

const EVAL_JOB_STATUSES: [EvaluationStatus; 3] = [
    EvaluationStatus::Fetching,
    EvaluationStatus::EvaluatingFlake,
    EvaluationStatus::EvaluatingDerivation,
];

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub assignments_closed: u64,
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
    pub builds_unpromoted: u64,
    pub evals_requeued: u64,
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

    report.evals_requeued = requeue_interrupted_evals(conn).await?;

    let clusters = crate::scheduling::cluster::recover_cluster_attempts(conn).await?;
    report.cluster_attempts_closed = clusters.attempts_closed;
    report.clusters_requeued = clusters.clusters_requeued;
    report.clusters_aborted = clusters.clusters_aborted;
    report.clusters_failed = clusters.clusters_failed;

    Ok(report)
}

async fn requeue_interrupted_evals<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    let ids: Vec<EvaluationId> = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(EVAL_JOB_STATUSES))
        .all(conn)
        .await?
        .into_iter()
        .map(|e| e.id)
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }

    let now = now();
    let res = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Queued))
        .col_expr(
            CEvaluation::WaitingReason,
            Expr::value(None::<serde_json::Value>),
        )
        .col_expr(CEvaluation::UpdatedAt, Expr::value(now))
        .col_expr(
            CEvaluation::GraphVersion,
            Expr::col(CEvaluation::GraphVersion).add(1),
        )
        .filter(CEvaluation::Id.is_in(ids.clone()))
        .exec(conn)
        .await?;

    let subjects: Vec<uuid::Uuid> = ids.iter().map(|e| e.into_inner()).collect();
    crate::status::record_phase_events(
        conn,
        crate::status::PhaseSubjectKind::Evaluation,
        &subjects,
        i32::from(EvaluationStatus::Queued) as i16,
        now,
    )
    .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(report.evals_requeued, 1);
        assert_eq!(report.cluster_attempts_closed, 1);
        assert_eq!(report.clusters_requeued, 1);
        assert_eq!(report.clusters_aborted, 0);

        let log = db.into_transaction_log();
        let eval_updates: Vec<_> = log
            .iter()
            .flat_map(|t| t.statements())
            .filter(|s| s.sql.starts_with("UPDATE \"evaluation\""))
            .collect();
        assert_eq!(eval_updates.len(), 1, "{eval_updates:?}");
        assert_eq!(
            eval_updates[0].values.as_ref().map(|v| v.0[0].clone()),
            Some(Value::Int(Some(i32::from(EvaluationStatus::Queued)))),
            "an interrupted evaluation is queued again, not aborted"
        );
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
    async fn nothing_is_reported_without_interrupted_work() {
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
        assert_eq!(report.evals_requeued, 0);
    }
}
