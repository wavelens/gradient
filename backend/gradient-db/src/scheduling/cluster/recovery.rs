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
use gradient_entity::cluster_member::{Column as CClusterMember, Entity as EClusterMember};
use gradient_entity::ids::{ClusterJobId, ClusterMemberId};
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, DeleteMany, EntityTrait, ExprTrait, QueryFilter,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requeue {
    No,
    WithinBudget,
    OutsideBudget,
}

fn retries_left() -> Expr {
    Expr::col(CClusterJob::Attempts)
        .sub(Expr::col(CClusterJob::AttemptsOutsideBudget))
        .lte(Expr::col(CClusterJob::RetryBudget))
}

fn counted_outside_budget() -> Expr {
    Expr::col(CClusterJob::AttemptsOutsideBudget).add(1)
}

pub async fn requeue_cluster_job<C: ConnectionTrait>(
    db: &C,
    cluster: ClusterJobId,
    requeue: Requeue,
) -> Result<bool, DbErr> {
    let update = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Queued)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CClusterJob::Id.eq(cluster))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Running))
        .filter(Expr::exists(dead_member_of_cluster()).not());
    let update = match requeue {
        Requeue::No => return Ok(false),
        Requeue::WithinBudget => update.filter(retries_left()),
        Requeue::OutsideBudget => {
            update.col_expr(CClusterJob::AttemptsOutsideBudget, counted_outside_budget())
        }
    };

    Ok(update.exec(db).await?.rows_affected == 1)
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

fn dissolvable() -> DeleteMany<EClusterJob> {
    let open_attempt = Query::select()
        .expr(Expr::val(1))
        .from(EClusterAttempt)
        .and_where(
            Expr::col((EClusterAttempt, CClusterAttempt::ClusterJob))
                .equals((EClusterJob, CClusterJob::Id)),
        )
        .and_where(Expr::col((EClusterAttempt, CClusterAttempt::FinishedAt)).is_null())
        .to_owned();

    EClusterJob::delete_many()
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Queued))
        .filter(CClusterJob::DissolveAfter.is_not_null())
        .filter(Expr::exists(open_attempt).not())
}

pub async fn dissolve_unplaced_clusters<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<ClusterJobId>, DbErr> {
    let dissolved = dissolvable()
        .filter(CClusterJob::DissolveAfter.lt(gradient_types::now()))
        .exec_with_returning(db)
        .await?;

    Ok(dissolved.into_iter().map(|c| c.id).collect())
}

pub async fn dissolve_split_cluster<C: ConnectionTrait>(
    db: &C,
    cluster: ClusterJobId,
) -> Result<bool, DbErr> {
    let res = dissolvable()
        .filter(CClusterJob::Id.eq(cluster))
        .exec(db)
        .await?;

    Ok(res.rows_affected == 1)
}

pub async fn drop_split_companions<C: ConnectionTrait>(
    db: &C,
    cluster: ClusterJobId,
    members: &[ClusterMemberId],
) -> Result<u64, DbErr> {
    let res = EClusterMember::delete_many()
        .filter(CClusterMember::ClusterJob.eq(cluster))
        .filter(CClusterMember::Id.is_in(members.iter().copied()))
        .filter(CClusterMember::Primary.eq(false))
        .filter(CClusterMember::EvalIndex.is_not_null())
        .exec(db)
        .await?;

    Ok(res.rows_affected)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClusterRecovery {
    pub attempts_closed: u64,
    pub clusters_requeued: u64,
    pub clusters_aborted: u64,
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
    let requeued = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Queued)),
        )
        .col_expr(CClusterJob::AttemptsOutsideBudget, counted_outside_budget())
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
    use crate::scheduling::cluster::test_rows::exec;
    use gradient_fixtures::mock::{all_statements, mock_db};
    use gradient_fixtures::shape::shape;
    use gradient_types::EvaluationId;

    #[tokio::test]
    async fn the_budget_counts_only_attempts_inside_it() {
        let db = mock_db().append_exec_results([exec(1)]).into_connection();

        let requeued = requeue_cluster_job(&db, ClusterJobId::now_v7(), Requeue::WithinBudget)
            .await
            .expect("requeue");

        assert!(requeued);
        let statements = all_statements(db.into_transaction_log());
        let sql = &statements[0].sql;
        assert!(
            sql.starts_with("UPDATE \"cluster_job\" SET \"status\" = $1"),
            "{sql}"
        );
        assert!(
            sql.contains("\"attempts\" - \"attempts_outside_budget\" <= \"retry_budget\""),
            "{sql}"
        );
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
    async fn an_unplaced_split_cluster_is_dissolved() {
        use gradient_entity::cluster_job::Model as MClusterJob;

        let cluster = ClusterJobId::now_v7();
        let db = mock_db()
            .append_query_results([vec![MClusterJob {
                id: cluster,
                ..Default::default()
            }]])
            .into_connection();

        let dissolved = dissolve_unplaced_clusters(&db).await.expect("dissolve");

        assert_eq!(dissolved, vec![cluster]);
        let statements = all_statements(db.into_transaction_log());
        let sql = &statements[0].sql;
        assert!(sql.starts_with("DELETE FROM \"cluster_job\""), "{sql}");
        assert!(sql.contains("\"dissolve_after\" < $"), "{sql}");
        assert!(sql.contains("NOT EXISTS(SELECT"), "{sql}");
        assert!(sql.contains("\"finished_at\" IS NULL"), "{sql}");
        let queued = format!("SmallInt(Some({}))", i16::from(ClusterJobStatus::Queued));
        assert!(format!("{:?}", statements[0].values).contains(&queued));
    }

    #[tokio::test]
    async fn finishing_moves_only_a_live_cluster() {
        let db = mock_db().append_exec_results([exec(0)]).into_connection();

        let finished = finish_cluster_job(&db, ClusterJobId::now_v7(), ClusterJobStatus::Failed)
            .await
            .expect("finish");

        assert!(!finished);
        let statements = all_statements(db.into_transaction_log());
        let sql = &statements[0].sql;
        assert!(
            sql.starts_with("UPDATE \"cluster_job\" SET \"status\" = $1"),
            "{sql}"
        );
        assert!(sql.contains("\"status\" IN ($"), "{sql}");
    }

    #[tokio::test]
    async fn startup_closes_every_open_attempt_then_requeues_live_clusters() {
        let db = mock_db()
            .append_exec_results([exec(2), exec(1), exec(1), exec(3)])
            .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
            .into_connection();

        let recovered = recover_cluster_attempts(&db).await.expect("recover");

        assert_eq!(
            recovered,
            ClusterRecovery {
                attempts_closed: 3,
                clusters_requeued: 3,
                clusters_aborted: 1,
            }
        );
        let statements = all_statements(db.into_transaction_log());
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
            sql[3].starts_with("UPDATE \"cluster_job\"")
                && sql[3].contains("\"attempts_outside_budget\" = \"attempts_outside_budget\" + ")
                && !sql[3].contains("EXISTS"),
            "{sql:?}"
        );
        let prepare_failed = format!(
            "SmallInt(Some({}))",
            i16::from(ClusterAttemptOutcome::PrepareFailed)
        );
        assert!(format!("{:?}", statements[0].values).contains(&prepare_failed));
    }

    #[tokio::test]
    async fn a_restart_outside_the_budget_requeues_and_counts_itself() {
        let (cluster, evaluation) = (ClusterJobId::now_v7(), EvaluationId::now_v7());
        let db = shape()
            .evaluation(evaluation)
            .cluster(
                cluster,
                true,
                3,
                ClusterJobStatus::Running,
                4,
                gradient_types::now(),
            )
            .evaluation_member_of(cluster, "server", true, None)
            .into_database()
            .await;
        let conn = db.connection();
        let row = || async { db.find::<EClusterJob>(cluster).await.expect("the cluster") };

        let spent = requeue_cluster_job(&conn, cluster, Requeue::WithinBudget).await;
        assert!(
            !spent.expect("requeue"),
            "four attempts spend a budget of 3"
        );
        let restarted = requeue_cluster_job(&conn, cluster, Requeue::OutsideBudget).await;
        assert!(restarted.expect("requeue"));
        let requeued = row().await;
        assert_eq!(
            (requeued.status, requeued.attempts_outside_budget),
            (ClusterJobStatus::Queued, 1)
        );

        db.execute("UPDATE cluster_job SET status = 1").await;
        let within = requeue_cluster_job(&conn, cluster, Requeue::WithinBudget).await;
        assert!(
            within.expect("requeue"),
            "one of four attempts is outside it"
        );
        let skipped = requeue_cluster_job(&conn, cluster, Requeue::No).await;
        assert!(!skipped.expect("requeue"));
        assert_eq!(row().await.attempts_outside_budget, 1);
        db.finish().await;
    }
}
