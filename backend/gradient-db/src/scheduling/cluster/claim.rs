/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::recovery::{Requeue, finish_cluster_job, requeue_cluster_job};
use crate::scheduling::assignment_record::{ClaimGate, abandon_open, claim_statement};
use chrono::NaiveDateTime;
use gradient_entity::cluster_attempt::{
    ClusterAttemptOutcome, Column as CClusterAttempt, Entity as EClusterAttempt,
};
use gradient_entity::cluster_job::{
    ClusterJobStatus, Column as CClusterJob, Entity as EClusterJob,
};
use gradient_entity::dispatched_job::{Column as CDispatchedJob, Model as MDispatchedJob};
use gradient_entity::ids::{ClusterAttemptId, ClusterJobId};
use sea_orm::sea_query::{Expr, InsertStatement, OnConflict, Query};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, ExprTrait, QueryFilter,
    TransactionTrait,
};

pub struct ClusterClaim {
    pub cluster: ClusterJobId,
    pub attempt: ClusterAttemptId,
    pub now: NaiveDateTime,
    pub members: Vec<(MDispatchedJob, ClaimGate)>,
}

pub async fn claim_cluster<C>(db: &C, claim: ClusterClaim) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    if claim.members.is_empty() {
        return Ok(false);
    }

    let txn = db.begin().await?;
    let claimed = claim_all(&txn, claim).await?;
    if claimed {
        txn.commit().await?;
    } else {
        txn.rollback().await?;
    }

    Ok(claimed)
}

async fn claim_all(txn: &DatabaseTransaction, claim: ClusterClaim) -> Result<bool, DbErr> {
    if !inserted(txn, open_attempt_statement(&claim)?).await? {
        return Ok(false);
    }

    for (row, gate) in claim.members {
        let row = MDispatchedJob {
            cluster_attempt: Some(claim.attempt),
            ..row
        };
        if !inserted(txn, claim_statement(row, gate)?).await? {
            return Ok(false);
        }
    }

    Ok(true)
}

async fn inserted(txn: &DatabaseTransaction, insert: InsertStatement) -> Result<bool, DbErr> {
    Ok(txn.execute(&insert).await?.rows_affected() == 1)
}

fn open_attempt_statement(claim: &ClusterClaim) -> Result<InsertStatement, DbErr> {
    let queued = Query::select()
        .expr(Expr::val(1))
        .from(EClusterJob)
        .and_where(Expr::col(CClusterJob::Id).eq(claim.cluster))
        .and_where(Expr::col(CClusterJob::Status).eq(ClusterJobStatus::Queued))
        .to_owned();
    let gated = Query::select()
        .exprs([
            Expr::val(claim.attempt),
            Expr::val(claim.cluster),
            Expr::val(claim.now),
        ])
        .and_where(Expr::exists(queued))
        .to_owned();
    let mut insert = Query::insert();
    insert
        .into_table(EClusterAttempt)
        .columns([
            CClusterAttempt::Id,
            CClusterAttempt::ClusterJob,
            CClusterAttempt::CreatedAt,
        ])
        .select_from(gated)
        .map_err(|e| DbErr::Custom(e.to_string()))?
        .on_conflict(
            OnConflict::column(CClusterAttempt::ClusterJob)
                .target_and_where(Expr::col(CClusterAttempt::FinishedAt).is_null())
                .do_nothing()
                .to_owned(),
        );

    Ok(insert)
}

pub async fn start_cluster_attempt<C>(
    db: &C,
    cluster: ClusterJobId,
    attempt: ClusterAttemptId,
) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let now = gradient_types::now();
    let txn = db.begin().await?;
    let running = EClusterJob::update_many()
        .col_expr(
            CClusterJob::Status,
            Expr::value(i16::from(ClusterJobStatus::Running)),
        )
        .col_expr(
            CClusterJob::Attempts,
            Expr::col(CClusterJob::Attempts).add(1),
        )
        .col_expr(CClusterJob::UpdatedAt, Expr::value(now))
        .filter(CClusterJob::Id.eq(cluster))
        .filter(CClusterJob::Status.eq(ClusterJobStatus::Queued))
        .exec(&txn)
        .await?
        .rows_affected
        == 1;
    if !running {
        close_attempt(&txn, attempt, ClusterAttemptOutcome::Aborted).await?;
        txn.commit().await?;
        return Ok(false);
    }

    let started = EClusterAttempt::update_many()
        .col_expr(CClusterAttempt::StartedAt, Expr::value(now))
        .filter(CClusterAttempt::Id.eq(attempt))
        .filter(CClusterAttempt::StartedAt.is_null())
        .filter(CClusterAttempt::FinishedAt.is_null())
        .exec(&txn)
        .await?
        .rows_affected
        == 1;
    if !started {
        txn.rollback().await?;
        return Ok(false);
    }
    txn.commit().await?;

    Ok(true)
}

pub async fn close_cluster_attempt<C>(
    db: &C,
    attempt: ClusterAttemptId,
    outcome: ClusterAttemptOutcome,
) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    let closed = close_attempt(&txn, attempt, outcome).await?;
    txn.commit().await?;

    Ok(closed)
}

/// Return whether the cluster job is in the queue for another attempt. Read the saved state instead if an earlier call already closed the attempt - for example, a call whose commit acknowledgement never arrived.
pub async fn resolve_cluster_attempt<C>(
    db: &C,
    cluster: ClusterJobId,
    attempt: ClusterAttemptId,
    outcome: ClusterAttemptOutcome,
    requeue: Requeue,
    finished: ClusterJobStatus,
) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    if !close_attempt(&txn, attempt, outcome).await? {
        let stored = EClusterJob::find_by_id(cluster).one(&txn).await?;
        txn.rollback().await?;
        return Ok(stored.is_some_and(|job| {
            matches!(
                job.status,
                ClusterJobStatus::Queued | ClusterJobStatus::Running
            )
        }));
    }
    let requeued = requeue_cluster_job(&txn, cluster, requeue).await?;
    if !requeued {
        finish_cluster_job(&txn, cluster, finished).await?;
    }
    txn.commit().await?;

    Ok(requeued)
}

pub async fn fail_prepare_attempt<C>(
    db: &C,
    cluster: ClusterJobId,
    attempt: ClusterAttemptId,
) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    let closed = close_attempt(&txn, attempt, ClusterAttemptOutcome::PrepareFailed).await?;
    if closed {
        EClusterJob::update_many()
            .col_expr(
                CClusterJob::Status,
                Expr::value(i16::from(ClusterJobStatus::Queued)),
            )
            .col_expr(CClusterJob::UpdatedAt, Expr::value(gradient_types::now()))
            .filter(CClusterJob::Id.eq(cluster))
            .filter(CClusterJob::Status.eq(ClusterJobStatus::Running))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;

    Ok(closed)
}

async fn close_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: ClusterAttemptId,
    outcome: ClusterAttemptOutcome,
) -> Result<bool, DbErr> {
    let closed = EClusterAttempt::update_many()
        .col_expr(
            CClusterAttempt::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(CClusterAttempt::Outcome, Expr::value(i16::from(outcome)))
        .filter(CClusterAttempt::Id.eq(attempt))
        .filter(CClusterAttempt::FinishedAt.is_null())
        .exec(db)
        .await?
        .rows_affected
        == 1;
    if closed {
        abandon_open(
            db,
            Some(Expr::col(CDispatchedJob::ClusterAttempt).eq(attempt)),
        )
        .await?;
    }

    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::cluster_attempt;
    use gradient_entity::dispatched_job::{self, DispatchedJobOutcome};
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_entity::ids::{DerivationBuildId, EvaluationId};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::shape::{build_assignment, eval_assignment, shape};
    use std::collections::HashMap;

    async fn eval_member(
        db: &TestDatabase,
        evaluation: EvaluationId,
    ) -> (MDispatchedJob, ClaimGate) {
        let row = eval_assignment(
            evaluation,
            db.project_of(evaluation).await,
            db.task_of(evaluation).await,
            gradient_types::now(),
        );

        (row, ClaimGate::Eval { evaluation })
    }

    async fn build_member(
        db: &TestDatabase,
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    ) -> (MDispatchedJob, ClaimGate) {
        let row = build_assignment(
            evaluation,
            db.project_of(evaluation).await,
            db.task_of(evaluation).await,
            shared_build,
            gradient_types::now(),
        );
        let gate = ClaimGate::Build {
            shared_build,
            output_names_to_substitute: None,
        };

        (row, gate)
    }

    fn claim_of(cluster: ClusterJobId, members: Vec<(MDispatchedJob, ClaimGate)>) -> ClusterClaim {
        ClusterClaim {
            cluster,
            attempt: ClusterAttemptId::now_v7(),
            now: gradient_types::now(),
            members,
        }
    }

    #[tokio::test]
    async fn claiming_cluster_opens_one_attempt_and_claims_all_members_or_rolls_back() {
        const BUILD: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-a.drv";
        let (open, lost, running, empty) = (
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
            ClusterJobId::now_v7(),
        );
        let (server, finished, active, fresh) = (
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
        );
        let now = gradient_types::now();
        let db = shape()
            .evaluation(server)
            .derivation(BUILD)
            .build_status(gradient_entity::build::BuildStatus::Queued)
            .evaluation(finished)
            .evaluation_status(EvaluationStatus::Completed)
            .evaluation(active)
            .evaluation(fresh)
            .cluster(open, true, 2, ClusterJobStatus::Queued, 0, now)
            .cluster(lost, true, 2, ClusterJobStatus::Queued, 0, now)
            .cluster(running, true, 2, ClusterJobStatus::Running, 1, now)
            .cluster(empty, true, 2, ClusterJobStatus::Queued, 0, now)
            .into_database()
            .await;
        let conn = db.connection();
        let build = db.shared_build(BUILD).await.id;
        let members = || async {
            vec![
                build_member(&db, server, build).await,
                eval_member(&db, active).await,
            ]
        };
        let opened = claim_of(open, members().await);
        let opened_attempt = opened.attempt;

        let won = claim_cluster(&conn, opened).await.expect("claim");
        let again = claim_cluster(&conn, claim_of(open, members().await))
            .await
            .expect("claim");
        let on_running = claim_cluster(&conn, claim_of(running, members().await))
            .await
            .expect("claim");
        let without_members = claim_cluster(&conn, claim_of(empty, Vec::new()))
            .await
            .expect("claim");
        let doomed = vec![
            eval_member(&db, fresh).await,
            eval_member(&db, finished).await,
        ];
        let with_lost_member = claim_cluster(&conn, claim_of(lost, doomed))
            .await
            .expect("claim");

        let attempts: Vec<_> = db
            .rows::<EClusterAttempt>()
            .await
            .into_iter()
            .map(|a| (a.id, a.cluster_job))
            .collect();
        let claimed: Vec<_> = db
            .rows::<dispatched_job::Entity>()
            .await
            .into_iter()
            .map(|a| a.cluster_attempt)
            .collect();
        db.finish().await;
        assert!(won);
        assert!(!again && !on_running && !without_members && !with_lost_member);
        assert_eq!(attempts, [(opened_attempt, open)]);
        assert_eq!(claimed, [Some(opened_attempt), Some(opened_attempt)]);
    }

    #[tokio::test]
    async fn cluster_attempt_lifecycle_start_close_abandoning_members_resolve_with_requeue_and_fail_prepare_requeues()
     {
        let now = gradient_types::now();
        let ids = || (ClusterJobId::now_v7(), ClusterAttemptId::now_v7());
        let (queued, queued_attempt) = ids();
        let (retry, retry_attempt) = ids();
        let (spent, spent_attempt) = ids();
        let (prepare, prepare_attempt) = ids();
        let (closing, closing_attempt) = ids();
        let mut seed = shape();
        for (cluster, attempt, status, attempts, budget, started) in [
            (queued, queued_attempt, ClusterJobStatus::Queued, 0, 2, None),
            (
                retry,
                retry_attempt,
                ClusterJobStatus::Running,
                1,
                2,
                Some(now),
            ),
            (
                spent,
                spent_attempt,
                ClusterJobStatus::Running,
                3,
                2,
                Some(now),
            ),
            (
                prepare,
                prepare_attempt,
                ClusterJobStatus::Running,
                1,
                2,
                None,
            ),
            (
                closing,
                closing_attempt,
                ClusterJobStatus::Running,
                1,
                2,
                Some(now),
            ),
        ] {
            seed = seed
                .evaluation(EvaluationId::now_v7())
                .cluster(cluster, true, budget, status, attempts, now)
                .evaluation_member_of(cluster, "server", true, None)
                .cluster_attempt(cluster, attempt, started, None, None);
        }
        let db = seed.into_database().await;
        let conn = db.connection();
        let resolve = |cluster, attempt| {
            resolve_cluster_attempt(
                &conn,
                cluster,
                attempt,
                ClusterAttemptOutcome::Failed,
                Requeue::WithinBudget,
                ClusterJobStatus::Failed,
            )
        };

        let started = start_cluster_attempt(&conn, queued, queued_attempt).await;
        let restarted = start_cluster_attempt(&conn, queued, queued_attempt).await;
        let requeued = resolve(retry, retry_attempt).await;
        let requeued_again = resolve(retry, retry_attempt).await;
        let exhausted = resolve(spent, spent_attempt).await;
        let exhausted_again = resolve(spent, spent_attempt).await;
        let prepared = fail_prepare_attempt(&conn, prepare, prepare_attempt).await;
        let prepared_again = fail_prepare_attempt(&conn, prepare, prepare_attempt).await;
        let closed =
            close_cluster_attempt(&conn, closing_attempt, ClusterAttemptOutcome::Failed).await;
        let closed_again =
            close_cluster_attempt(&conn, closing_attempt, ClusterAttemptOutcome::Failed).await;

        let mut states = HashMap::new();
        for cluster in [queued, retry, spent, prepare, closing] {
            let row = db.find::<EClusterJob>(cluster).await.expect("the cluster");
            states.insert(cluster, (row.status, row.attempts));
        }
        let attempts: HashMap<_, _> = db
            .rows::<cluster_attempt::Entity>()
            .await
            .into_iter()
            .map(|a| (a.id, (a.started_at.is_some(), a.outcome)))
            .collect();
        let abandoned = db
            .rows::<dispatched_job::Entity>()
            .await
            .into_iter()
            .filter(|a| a.outcome == Some(DispatchedJobOutcome::Abandoned))
            .filter_map(|a| a.cluster_attempt)
            .collect::<std::collections::HashSet<_>>();
        db.finish().await;
        assert!(started.expect("start"));
        assert!(
            !restarted.expect("start"),
            "a started attempt starts nothing"
        );
        assert!(requeued.expect("resolve"));
        assert!(
            requeued_again.expect("resolve"),
            "reports the stored requeue"
        );
        assert!(!exhausted.expect("resolve"));
        assert!(!exhausted_again.expect("resolve"));
        assert!(prepared.expect("prepare"));
        assert!(!prepared_again.expect("prepare"));
        assert!(closed.expect("close"));
        assert!(!closed_again.expect("close"));
        assert_eq!(states[&queued], (ClusterJobStatus::Running, 1));
        assert_eq!(states[&retry].0, ClusterJobStatus::Queued);
        assert_eq!(states[&spent].0, ClusterJobStatus::Failed);
        assert_eq!(states[&prepare].0, ClusterJobStatus::Queued);
        assert_eq!(states[&closing].0, ClusterJobStatus::Running);
        assert_eq!(
            attempts[&queued_attempt],
            (true, Some(ClusterAttemptOutcome::Aborted))
        );
        assert_eq!(
            attempts[&prepare_attempt].1,
            Some(ClusterAttemptOutcome::PrepareFailed)
        );
        assert_eq!(
            attempts[&closing_attempt].1,
            Some(ClusterAttemptOutcome::Failed)
        );
        assert_eq!(
            abandoned,
            [
                queued_attempt,
                retry_attempt,
                spent_attempt,
                prepare_attempt,
                closing_attempt
            ]
            .into()
        );
    }
}
