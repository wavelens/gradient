/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::membership::{abort_dead_queued_clusters, dead_member_of_cluster};
use gradient_entity::cluster_attempt::{
    ClusterAttemptOutcome, Column as CClusterAttempt, Entity as EClusterAttempt,
};
use gradient_entity::cluster_job::{
    ClusterJobStatus, Column as CClusterJob, Entity as EClusterJob,
};
use gradient_entity::ids::ClusterJobId;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, ExprTrait, QueryFilter};

pub async fn requeue_cluster_job<C: ConnectionTrait>(
    db: &C,
    cluster: ClusterJobId,
) -> Result<bool, DbErr> {
    let res = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Queued)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Id.eq(cluster))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Running))
        .filter(Expr::col(CClusterJob::Attempts).lt(Expr::col(CClusterJob::RetryBudget)))
        .filter(Expr::exists(dead_member_of_cluster()).not())
        .exec(db)
        .await?;

    Ok(res.rows_affected == 1)
}

pub async fn finish_cluster_job<C: ConnectionTrait>(
    db: &C,
    cluster: ClusterJobId,
    status: ClusterJobStatus,
) -> Result<bool, DbErr> {
    let res = EClusterJob::update_many()
        .col_expr(CClusterJob::Status, Expr::value(i16::from(status)))
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Id.eq(cluster))
        .filter(CClusterJob::Status.is_in([ClusterJobStatus::Queued, ClusterJobStatus::Running]))
        .exec(db)
        .await?;

    Ok(res.rows_affected == 1)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClusterRecovery {
    pub attempts_closed: u64,
    pub clusters_requeued: u64,
    pub clusters_aborted: u64,
    pub clusters_failed: u64,
}

pub async fn recover_cluster_attempts<C: ConnectionTrait>(
    conn: &C,
) -> Result<ClusterRecovery, DbErr> {
    let unstarted = close_open_attempts(conn, true, ClusterAttemptOutcome::PrepareFailed).await?;
    let started = close_open_attempts(conn, false, ClusterAttemptOutcome::Aborted).await?;
    let aborted = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Aborted)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Running))
        .filter(Expr::exists(dead_member_of_cluster()))
        .exec(conn)
        .await?
        .rows_affected;
    let failed = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Failed)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Running))
        .filter(Expr::col(CClusterJob::Attempts).gte(Expr::col(CClusterJob::RetryBudget)))
        .exec(conn)
        .await?
        .rows_affected;
    let requeued = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Queued)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Running))
        .exec(conn)
        .await?
        .rows_affected;
    let dead_queued = abort_dead_queued_clusters(conn).await?.len() as u64;

    Ok(ClusterRecovery {
        attempts_closed: unstarted + started,
        clusters_requeued: requeued,
        clusters_aborted: aborted + dead_queued,
        clusters_failed: failed,
    })
}

async fn close_open_attempts<C: ConnectionTrait>(
    conn: &C,
    unstarted: bool,
    outcome: ClusterAttemptOutcome,
) -> Result<u64, DbErr> {
    let started = if unstarted {
        CClusterAttempt::StartedAt.is_null()
    } else {
        CClusterAttempt::StartedAt.is_not_null()
    };
    let res = EClusterAttempt::update_many()
        .col_expr(
            CClusterAttempt::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(CClusterAttempt::Outcome, Expr::value(i16::from(outcome)))
        .filter(CClusterAttempt::FinishedAt.is_null())
        .filter(started)
        .exec(conn)
        .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduling::cluster::test_rows::{exec, logged};

    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn a_cluster_requeues_only_within_its_budget_with_live_members() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1)])
            .into_connection();

        let requeued = requeue_cluster_job(&db, ClusterJobId::now_v7())
            .await
            .expect("requeue");

        assert!(requeued);
        let statements = logged(db);
        let sql = &statements[0].sql;
        assert!(
            sql.starts_with("UPDATE \"cluster_job\" SET \"status\" = $1"),
            "{sql}"
        );
        assert!(sql.contains("\"attempts\" < \"retry_budget\""), "{sql}");
        assert!(sql.contains("NOT EXISTS(SELECT"), "{sql}");
        assert!(sql.contains("FROM \"cluster_member\""), "{sql}");
        assert!(
            sql.contains("\"cluster_member\".\"cluster_job\" = \"cluster_job\".\"id\""),
            "{sql}"
        );
        let queued = format!("SmallInt(Some({}))", i16::from(ClusterJobStatus::Queued));
        let running = format!("SmallInt(Some({}))", i16::from(ClusterJobStatus::Running));
        let values = format!("{:?}", statements[0].values);
        assert!(
            values.contains(&queued) && values.contains(&running),
            "{values}"
        );
    }

    #[tokio::test]
    async fn finishing_moves_only_a_live_cluster() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0)])
            .into_connection();

        let finished = finish_cluster_job(&db, ClusterJobId::now_v7(), ClusterJobStatus::Failed)
            .await
            .expect("finish");

        assert!(!finished);
        let statements = logged(db);
        let sql = &statements[0].sql;
        assert!(
            sql.starts_with("UPDATE \"cluster_job\" SET \"status\" = $1"),
            "{sql}"
        );
        assert!(sql.contains("\"status\" IN ($"), "{sql}");
    }

    #[tokio::test]
    async fn startup_closes_every_open_attempt_then_requeues_live_clusters() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(2), exec(1), exec(1), exec(1), exec(3)])
            .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
            .into_connection();

        let recovered = recover_cluster_attempts(&db).await.expect("recover");

        assert_eq!(
            recovered,
            ClusterRecovery {
                attempts_closed: 3,
                clusters_requeued: 3,
                clusters_aborted: 1,
                clusters_failed: 1,
            }
        );
        let statements = logged(db);
        let sql: Vec<&str> = statements.iter().map(|s| s.sql.as_str()).collect();
        assert!(
            sql[0].starts_with("UPDATE \"cluster_attempt\"")
                && sql[0].contains("\"started_at\" IS NULL"),
            "{sql:?}"
        );
        assert!(
            sql[1].starts_with("UPDATE \"cluster_attempt\"")
                && sql[1].contains("\"started_at\" IS NOT NULL"),
            "{sql:?}"
        );
        assert!(
            sql[2].starts_with("UPDATE \"cluster_job\"") && sql[2].contains("EXISTS(SELECT"),
            "{sql:?}"
        );
        assert!(
            sql[3].starts_with("UPDATE \"cluster_job\"") && !sql[3].contains("EXISTS"),
            "{sql:?}"
        );
        let prepare_failed = format!(
            "SmallInt(Some({}))",
            i16::from(ClusterAttemptOutcome::PrepareFailed)
        );
        assert!(format!("{:?}", statements[0].values).contains(&prepare_failed));
    }
}
