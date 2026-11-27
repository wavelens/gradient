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

pub const SPLIT_PLACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    let now = gradient_types::now();
    let place_deadline = chrono::TimeDelta::from_std(SPLIT_PLACE_TIMEOUT)
        .map_err(|e| DbErr::Custom(e.to_string()))?;
    let update = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Queued)),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(now))
        .col_expr(
            CClusterJob::DissolveAfter,
            Expr::case(
                Expr::col(CClusterJob::DissolveAfter).is_not_null(),
                Expr::value(now + place_deadline),
            )
            .finally(Expr::col(CClusterJob::DissolveAfter))
            .into(),
        )
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
    use gradient_entity::ids::ClusterAttemptId;
    use gradient_fixtures::shape::shape;
    use gradient_types::EvaluationId;

    #[tokio::test]
    async fn finish_moves_only_live_clusters_and_dissolve_removes_only_queued_unplaced_clusters_past_deadline_without_open_attempt()
     {
        let now = gradient_types::now();
        let (past, future) = (
            now - chrono::Duration::hours(1),
            now + chrono::Duration::hours(1),
        );
        let ids: Vec<ClusterJobId> = (0..7).map(|_| ClusterJobId::now_v7()).collect();
        let [running, done, due, not_due, plain, placed, running_due] = ids[..] else {
            unreachable!()
        };
        let mut seed = shape().evaluation(EvaluationId::now_v7());
        for (cluster, status, dissolve_after) in [
            (running, ClusterJobStatus::Running, None),
            (done, ClusterJobStatus::Completed, None),
            (due, ClusterJobStatus::Queued, Some(past)),
            (not_due, ClusterJobStatus::Queued, Some(future)),
            (plain, ClusterJobStatus::Queued, None),
            (placed, ClusterJobStatus::Queued, Some(past)),
            (running_due, ClusterJobStatus::Running, Some(past)),
        ] {
            seed = seed
                .cluster(cluster, false, 2, status, 0, now)
                .cluster_option(cluster, |c| c.dissolve_after = dissolve_after);
        }
        let db = seed
            .cluster_attempt(placed, ClusterAttemptId::now_v7(), None, None, None)
            .into_database()
            .await;
        let conn = db.connection();

        let finished_live = finish_cluster_job(&conn, running, ClusterJobStatus::Failed).await;
        let finished_done = finish_cluster_job(&conn, done, ClusterJobStatus::Failed).await;
        let dissolved = dissolve_unplaced_clusters(&conn).await;
        let split_not_due = dissolve_split_cluster(&conn, not_due).await;
        let split_plain = dissolve_split_cluster(&conn, plain).await;
        let split_placed = dissolve_split_cluster(&conn, placed).await;
        let split_running = dissolve_split_cluster(&conn, running_due).await;

        let mut left: Vec<_> = db
            .rows::<EClusterJob>()
            .await
            .into_iter()
            .map(|c| (c.id, c.status))
            .collect();
        left.sort_by_key(|(id, _)| *id);
        db.finish().await;
        assert!(finished_live.expect("finish"));
        assert!(!finished_done.expect("finish"));
        assert_eq!(dissolved.expect("dissolve"), [due]);
        assert!(split_not_due.expect("dissolve"));
        assert!(!split_plain.expect("dissolve"));
        assert!(!split_placed.expect("dissolve"));
        assert!(!split_running.expect("dissolve"));
        let mut expected = vec![
            (running, ClusterJobStatus::Failed),
            (done, ClusterJobStatus::Completed),
            (plain, ClusterJobStatus::Queued),
            (placed, ClusterJobStatus::Queued),
            (running_due, ClusterJobStatus::Running),
        ];
        expected.sort_by_key(|(id, _)| *id);
        assert_eq!(left, expected);
    }

    #[tokio::test]
    async fn startup_recovery_closes_open_attempts_requeues_running_and_aborts_dead_member() {
        use gradient_entity::evaluation::EvaluationStatus;

        let now = gradient_types::now();
        let (alive, dead_running, dead_queued, idle) = (
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
        );
        let (started, unstarted, finished) = (
            ClusterAttemptId::now_v7(),
            ClusterAttemptId::now_v7(),
            ClusterAttemptId::now_v7(),
        );
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cluster(alive, true, 3, ClusterJobStatus::Running, 1, now)
            .evaluation_member_of(alive, "server", true, None)
            .cluster_attempt(alive, started, Some(now), None, None)
            .evaluation(EvaluationId::now_v7())
            .evaluation_status(EvaluationStatus::Completed)
            .cluster(dead_running, true, 3, ClusterJobStatus::Running, 1, now)
            .evaluation_member_of(dead_running, "server", true, None)
            .cluster_attempt(dead_running, unstarted, None, None, None)
            .evaluation(EvaluationId::now_v7())
            .evaluation_status(EvaluationStatus::Completed)
            .cluster(dead_queued, true, 3, ClusterJobStatus::Queued, 0, now)
            .evaluation_member_of(dead_queued, "server", true, None)
            .evaluation(EvaluationId::now_v7())
            .cluster(idle, true, 3, ClusterJobStatus::Queued, 1, now)
            .evaluation_member_of(idle, "server", true, None)
            .cluster_attempt(
                idle,
                finished,
                Some(now),
                Some(now),
                Some(ClusterAttemptOutcome::Failed),
            )
            .into_database()
            .await;

        let recovered = recover_cluster_attempts(&db.connection())
            .await
            .expect("recover");

        let status = |cluster| {
            let db = &db;
            async move {
                let row = db.find::<EClusterJob>(cluster).await.expect("the cluster");
                (row.status, row.attempts_outside_budget)
            }
        };
        let outcomes: std::collections::HashMap<_, _> = db
            .rows::<EClusterAttempt>()
            .await
            .into_iter()
            .map(|a| (a.id, a.outcome))
            .collect();
        let statuses = [
            status(alive).await,
            status(dead_running).await,
            status(dead_queued).await,
            status(idle).await,
        ];
        db.finish().await;
        assert_eq!(
            recovered,
            ClusterRecovery {
                attempts_closed: 2,
                clusters_requeued: 1,
                clusters_aborted: 2,
            }
        );
        assert_eq!(
            statuses,
            [
                (ClusterJobStatus::Queued, 1),
                (ClusterJobStatus::Aborted, 0),
                (ClusterJobStatus::Aborted, 0),
                (ClusterJobStatus::Queued, 0),
            ]
        );
        assert_eq!(outcomes[&started], Some(ClusterAttemptOutcome::Aborted));
        assert_eq!(
            outcomes[&unstarted],
            Some(ClusterAttemptOutcome::PrepareFailed)
        );
        assert_eq!(outcomes[&finished], Some(ClusterAttemptOutcome::Failed));
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

    #[tokio::test]
    async fn a_requeued_split_cluster_gets_a_fresh_place_deadline() {
        let (split, plain, evaluation) = (
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
            EvaluationId::now_v7(),
        );
        let long_ago = gradient_types::now() - chrono::Duration::minutes(10);
        let db = shape()
            .evaluation(evaluation)
            .cluster(split, false, 2, ClusterJobStatus::Running, 1, long_ago)
            .cluster_option(split, |cluster| {
                cluster.dissolve_after = Some(long_ago + chrono::Duration::seconds(30));
            })
            .split_member_of(split, "primary", 0)
            .evaluation(EvaluationId::now_v7())
            .cluster(plain, false, 2, ClusterJobStatus::Running, 1, long_ago)
            .evaluation_member_of(plain, "server", true, None)
            .into_database()
            .await;
        let conn = db.connection();

        for cluster in [split, plain] {
            let requeued = requeue_cluster_job(&conn, cluster, Requeue::WithinBudget).await;
            assert!(requeued.expect("requeue"));
        }

        assert_eq!(
            dissolve_unplaced_clusters(&conn).await.expect("dissolve"),
            []
        );
        let deadline = db
            .find::<EClusterJob>(split)
            .await
            .and_then(|row| row.dissolve_after)
            .expect("the split keeps a deadline");
        assert!(deadline > gradient_types::now());
        let plain_row = db.find::<EClusterJob>(plain).await.expect("the cluster");
        assert_eq!(plain_row.dissolve_after, None);
        db.finish().await;
    }
}
