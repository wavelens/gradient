/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::scheduling::assignment_record::{
    abandon_open_cluster_assignments, build_job_key_sql, eval_job_key_sql,
    no_open_assignment_predicate, open_assignment_keys,
};
use crate::sql::StatusWrite;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use gradient_types::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptAssignment {
    pub assignment_id: DispatchedJobId,
    pub worker_id: String,
    pub job_key: String,
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub kept: Vec<KeptAssignment>,
    pub attempts_aborted: u64,
    pub builds_requeued: u64,
    pub builds_unpromoted: u64,
    pub evals_requeued: u64,
    pub cluster_attempts_closed: u64,
    pub clusters_requeued: u64,
    pub clusters_aborted: u64,
    pub clusters_failed: u64,
    pub connections_closed: u64,
}

fn requeue_mid_flight_sql() -> String {
    format!(
        "UPDATE derivation_build SET status = {queued}, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE status = {building} AND {unassigned} RETURNING derivation",
        queued = crate::sql::status::to(BuildStatus::REQUEUE_MID_FLIGHT),
        building = crate::sql::status::from(BuildStatus::REQUEUE_MID_FLIGHT),
        unassigned = no_open_assignment_predicate(&build_job_key_sql("derivation_build.id")),
    )
}

crate::sql_fn! {
    REQUEUE_MID_FLIGHT = requeue_mid_flight_sql,
        params = [],
        tier = Sweep,
        status_write = StatusWrite::build(BuildStatus::REQUEUE_MID_FLIGHT);
}

pub async fn recover_interrupted_work<C: ConnectionTrait>(
    conn: &C,
) -> Result<RecoveryReport, DbErr> {
    // Cluster rows must close before the cluster recovery below, and every other open row keeps
    // its work as it is until its worker reports it again or the reclaim grace ends.
    abandon_open_cluster_assignments(conn).await?;
    let mut report = RecoveryReport {
        kept: kept_assignments(conn).await?,
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
        .filter(Expr::cust(no_open_assignment_predicate(
            &build_job_key_sql("\"build_attempt\".\"derivation_build\""),
        )))
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

    report.connections_closed = gradient_entity::worker_connection::Entity::update_many()
        .col_expr(
            gradient_entity::worker_connection::Column::DisconnectedAt,
            Expr::value(now),
        )
        .filter(gradient_entity::worker_connection::Column::DisconnectedAt.is_null())
        .exec(conn)
        .await?
        .rows_affected;

    Ok(report)
}

async fn kept_assignments<C: ConnectionTrait>(conn: &C) -> Result<Vec<KeptAssignment>, DbErr> {
    Ok(open_assignment_keys(conn)
        .await?
        .into_iter()
        .map(|(assignment_id, job_key, worker_id)| KeptAssignment {
            assignment_id,
            worker_id,
            job_key,
        })
        .collect())
}

async fn requeue_interrupted_evals<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    let ids: Vec<EvaluationId> = EEvaluation::find()
        .filter(
            CEvaluation::Status.is_in(EvaluationStatus::REQUEUE_INTERRUPTED.from.iter().copied()),
        )
        .filter(Expr::cust(no_open_assignment_predicate(&eval_job_key_sql(
            "\"evaluation\".\"id\"",
        ))))
        .all(conn)
        .await?
        .into_iter()
        .map(|e| e.id)
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }

    let now = now();
    let res = crate::status::evaluation_change(EvaluationStatus::REQUEUE_INTERRUPTED)
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
        i16::from(EvaluationStatus::Queued),
        now,
    )
    .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;

    use gradient_entity::evaluation::Model as MEval;
    use gradient_entity::ids::{CommitId, EvaluationId, TaskId};
    use sea_orm::{DatabaseConnection, MockExecResult, Value};
    use std::collections::BTreeMap;

    type Row = BTreeMap<String, Value>;

    fn derivation_row(derivation: DerivationId) -> Row {
        BTreeMap::from([(
            "derivation".to_owned(),
            Value::from(derivation.into_inner()),
        )])
    }

    fn unpromoted_row(derivation: DerivationId) -> Row {
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

    fn kept_row(kept: &KeptAssignment) -> Row {
        BTreeMap::from([
            (
                "id".to_owned(),
                Value::from(kept.assignment_id.into_inner()),
            ),
            ("job_id".to_owned(), Value::from(kept.job_key.clone())),
            ("worker_id".to_owned(), Value::from(kept.worker_id.clone())),
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

    fn affected(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn idle_db(kept: Vec<Row>) -> DatabaseConnection {
        mock_db()
            .append_exec_results([affected(0), affected(0)])
            .append_query_results([kept])
            .append_query_results([Vec::<Row>::new()])
            .append_query_results([Vec::<MEval>::new()])
            .append_exec_results([0, 0, 0, 0, 0].map(affected))
            .append_query_results([Vec::<Row>::new()])
            .append_exec_results([affected(0)])
            .into_connection()
    }

    async fn recovered_sql(db: DatabaseConnection) -> Vec<String> {
        recover_interrupted_work(&db).await.unwrap();

        db.into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| s.sql.clone())
            .collect()
    }

    #[tokio::test]
    async fn all_operations_populate_report() {
        let task_id = TaskId::now_v7();
        let mid_flight = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([affected(1), affected(3)])
            .append_query_results([Vec::<Row>::new()])
            .append_query_results([vec![
                derivation_row(mid_flight),
                derivation_row(DerivationId::now_v7()),
            ]])
            .append_query_results([vec![unpromoted_row(mid_flight)]])
            .append_exec_results([affected(1)])
            .append_query_results([vec![eval_row(EvaluationStatus::Fetching, Some(task_id))]])
            .append_exec_results([affected(1)])
            .append_query_results([crate::test_ctx::inserted_phase_event()])
            .append_exec_results([1, 0, 0, 0, 1].map(affected))
            .append_query_results([Vec::<Row>::new()])
            .append_exec_results([affected(0)])
            .into_connection();

        let report = recover_interrupted_work(&db).await.unwrap();

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
    async fn open_assignments_are_kept_and_cluster_ones_close() {
        let kept = KeptAssignment {
            assignment_id: DispatchedJobId::now_v7(),
            worker_id: "w1".into(),
            job_key: format!("build:{}", DerivationBuildId::now_v7()),
        };
        let db = idle_db(vec![kept_row(&kept)]);

        let report = recover_interrupted_work(&db).await.unwrap();

        assert_eq!(report.kept, vec![kept]);
        let sql: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| s.sql.clone())
            .collect();
        let closes: Vec<_> = sql
            .iter()
            .filter(|s| s.starts_with("UPDATE \"dispatched_job\""))
            .collect();
        assert_eq!(closes.len(), 1, "{sql:?}");
        assert!(closes[0].contains("cluster_attempt IS NOT NULL"), "{sql:?}");
        let close = sql
            .iter()
            .position(|s| s.starts_with("UPDATE \"dispatched_job\""))
            .unwrap();
        let select = sql
            .iter()
            .position(|s| s.starts_with("SELECT") && s.contains("FROM \"dispatched_job\""))
            .expect("recovery reads the open rows it keeps");
        assert!(close < select, "{sql:?}");
    }

    #[tokio::test]
    async fn only_builds_without_an_open_assignment_are_requeued() {
        let sql = recovered_sql(idle_db(Vec::new())).await;

        let requeue = sql
            .iter()
            .find(|s| s.contains("UPDATE derivation_build SET status"))
            .expect("recovery requeues the mid-flight shared builds");
        assert!(
            requeue.contains(&no_open_assignment_predicate(&build_job_key_sql(
                "derivation_build.id"
            ))),
            "{requeue}"
        );
        let attempts = sql
            .iter()
            .find(|s| s.starts_with("UPDATE \"build_attempt\""))
            .expect("recovery aborts running attempts");
        assert!(
            attempts.contains(&no_open_assignment_predicate(&build_job_key_sql(
                "\"build_attempt\".\"derivation_build\""
            ))),
            "{attempts}"
        );
        let evals = sql
            .iter()
            .find(|s| s.contains("FROM \"evaluation\""))
            .expect("recovery reads the interrupted evaluations");
        assert!(
            evals.contains(&no_open_assignment_predicate(&eval_job_key_sql(
                "\"evaluation\".\"id\""
            ))),
            "{evals}"
        );
    }

    #[tokio::test]
    async fn clusters_recover_after_the_evaluations_they_hold() {
        let sql = recovered_sql(idle_db(Vec::new())).await;

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
        let report = recover_interrupted_work(&idle_db(Vec::new()))
            .await
            .unwrap();

        assert!(report.kept.is_empty());
        assert_eq!(report.attempts_aborted, 0);
        assert_eq!(report.builds_requeued, 0);
        assert_eq!(report.builds_unpromoted, 0);
        assert_eq!(report.evals_requeued, 0);
    }
}
