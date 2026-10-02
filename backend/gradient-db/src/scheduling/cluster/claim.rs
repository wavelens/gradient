/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::recovery::{finish_cluster_job, requeue_cluster_job};
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
    let started = EClusterAttempt::update_many()
        .col_expr(CClusterAttempt::StartedAt, Expr::value(now))
        .filter(CClusterAttempt::Id.eq(attempt))
        .filter(CClusterAttempt::StartedAt.is_null())
        .filter(CClusterAttempt::FinishedAt.is_null())
        .exec(&txn)
        .await?
        .rows_affected
        == 1;
    if started {
        EClusterJob::update_many()
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
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;

    Ok(started)
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

pub async fn resolve_cluster_attempt<C>(
    db: &C,
    cluster: ClusterJobId,
    attempt: ClusterAttemptId,
    outcome: ClusterAttemptOutcome,
    retry: bool,
    finished: ClusterJobStatus,
) -> Result<Option<bool>, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    if !close_attempt(&txn, attempt, outcome).await? {
        txn.rollback().await?;
        return Ok(None);
    }
    let requeued = retry && requeue_cluster_job(&txn, cluster).await?;
    if !requeued {
        finish_cluster_job(&txn, cluster, finished).await?;
    }
    txn.commit().await?;

    Ok(Some(requeued))
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
    use crate::scheduling::cluster::test_rows::{exec, logged};
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use gradient_entity::ids::{DerivationBuildId, DispatchedJobId, EvaluationId};
    use sea_orm::{DatabaseBackend, MockDatabase, Statement};

    fn member(job_id: &str, gate: ClaimGate) -> (MDispatchedJob, ClaimGate) {
        let row = MDispatchedJob {
            id: DispatchedJobId::now_v7(),
            job_id: Some(job_id.to_owned()),
            worker_id: "w1".into(),
            ..Default::default()
        };

        (row, gate)
    }

    fn claim() -> ClusterClaim {
        ClusterClaim {
            cluster: ClusterJobId::now_v7(),
            attempt: ClusterAttemptId::now_v7(),
            now: chrono::Utc::now().naive_utc(),
            members: vec![
                member(
                    "build:a",
                    ClaimGate::Build {
                        shared_build: DerivationBuildId::now_v7(),
                        substitute: false,
                    },
                ),
                member(
                    "eval:b",
                    ClaimGate::Eval {
                        evaluation: EvaluationId::now_v7(),
                    },
                ),
            ],
        }
    }

    async fn run(results: Vec<u64>, claim: ClusterClaim) -> (bool, Vec<Statement>) {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(results.into_iter().map(exec))
            .into_connection();
        let won = claim_cluster(&db, claim).await.expect("claim");

        (won, logged(db))
    }

    fn inserts_into<'a>(statements: &'a [Statement], table: &str) -> Vec<&'a Statement> {
        let prefix = format!("INSERT INTO \"{table}\"");
        statements
            .iter()
            .filter(|s| s.sql.starts_with(&prefix))
            .collect()
    }

    async fn closed(result: u64, outcome: ClusterAttemptOutcome) -> (bool, Vec<Statement>) {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(result), exec(2)])
            .into_connection();
        let closed = close_cluster_attempt(&db, ClusterAttemptId::now_v7(), outcome)
            .await
            .expect("close");

        (closed, logged(db))
    }

    fn updates<'a>(statements: &'a [Statement], table: &str) -> Vec<&'a Statement> {
        let prefix = format!("UPDATE \"{table}\"");
        statements
            .iter()
            .filter(|s| s.sql.starts_with(&prefix))
            .collect()
    }

    async fn started(first: u64) -> (bool, Vec<Statement>) {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(first), exec(1)])
            .into_connection();
        let started =
            start_cluster_attempt(&db, ClusterJobId::now_v7(), ClusterAttemptId::now_v7())
                .await
                .expect("start");

        (started, logged(db))
    }

    #[tokio::test]
    async fn a_cluster_claim_opens_its_attempt_then_claims_every_member() {
        let claim = claim();
        let attempt = claim.attempt;
        let (won, statements) = run(vec![1, 1, 1], claim).await;

        assert!(won);
        let opened = inserts_into(&statements, "cluster_attempt");
        let sql = &opened.first().expect("attempt insert").sql;
        assert!(sql.contains("FROM \"cluster_job\""), "{sql}");
        assert!(
            sql.contains("ON CONFLICT (\"cluster_job\") WHERE \"finished_at\" IS NULL DO NOTHING"),
            "{sql}"
        );
        let members = inserts_into(&statements, "dispatched_job");
        assert_eq!(members.len(), 2);
        for m in members {
            let values = format!("{:?}", m.values);
            assert!(
                values.contains(&attempt.into_inner().to_string()),
                "{values}"
            );
        }
        assert!(
            statements.iter().any(|s| s.sql == "COMMIT"),
            "{statements:?}"
        );
    }

    #[tokio::test]
    async fn a_claim_without_members_opens_no_attempt() {
        let claim = ClusterClaim {
            members: Vec::new(),
            ..claim()
        };
        let (won, statements) = run(vec![1], claim).await;

        assert!(!won);
        assert!(statements.is_empty(), "{statements:?}");
    }

    #[tokio::test]
    async fn a_cluster_already_open_claims_no_member() {
        let (won, statements) = run(vec![0], claim()).await;

        assert!(!won);
        assert!(inserts_into(&statements, "dispatched_job").is_empty());
    }

    #[tokio::test]
    async fn a_lost_member_claim_rolls_the_attempt_back() {
        let (won, statements) = run(vec![1, 1, 0], claim()).await;

        assert!(!won);
        assert!(
            statements.iter().any(|s| s.sql == "ROLLBACK"),
            "{statements:?}"
        );
        assert!(
            !statements.iter().any(|s| s.sql == "COMMIT"),
            "{statements:?}"
        );
    }

    #[tokio::test]
    async fn closing_an_attempt_abandons_its_open_members() {
        let (closed, statements) = closed(1, ClusterAttemptOutcome::PrepareFailed).await;

        assert!(closed);
        let members = updates(&statements, "dispatched_job");
        let sql = &members.first().expect("member close").sql;
        assert!(sql.contains("\"cluster_attempt\" ="), "{sql}");
        assert!(sql.contains("\"finished_at\" IS NULL"), "{sql}");
        let abandoned = format!(
            "SmallInt(Some({}))",
            i16::from(DispatchedJobOutcome::Abandoned)
        );
        assert!(format!("{:?}", members[0].values).contains(&abandoned));
    }

    #[tokio::test]
    async fn closing_touches_only_open_rows() {
        let (closed, statements) = closed(0, ClusterAttemptOutcome::Failed).await;

        assert!(!closed);
        let attempt = updates(&statements, "cluster_attempt");
        let sql = &attempt.first().expect("attempt close").sql;
        assert!(sql.contains("\"finished_at\" IS NULL"), "{sql}");
        assert!(updates(&statements, "dispatched_job").is_empty());
    }

    #[tokio::test]
    async fn starting_an_open_attempt_runs_its_cluster() {
        let (started, statements) = started(1).await;

        assert!(started);
        let attempt = &statements
            .iter()
            .find(|s| s.sql.starts_with("UPDATE \"cluster_attempt\""))
            .expect("attempt")
            .sql;
        assert!(attempt.contains("\"started_at\" IS NULL"), "{attempt}");
        assert!(attempt.contains("\"finished_at\" IS NULL"), "{attempt}");
        let job = statements
            .iter()
            .find(|s| s.sql.starts_with("UPDATE \"cluster_job\""))
            .expect("cluster");
        assert!(
            job.sql.contains("\"attempts\" = \"attempts\" + $"),
            "{}",
            job.sql
        );
        let running = format!("SmallInt(Some({}))", i16::from(ClusterJobStatus::Running));
        assert!(format!("{:?}", job.values).contains(&running));
    }

    #[tokio::test]
    async fn a_started_or_closed_attempt_starts_nothing() {
        let (started, statements) = started(0).await;

        assert!(!started);
        assert!(
            !statements
                .iter()
                .any(|s| s.sql.starts_with("UPDATE \"cluster_job\""))
        );
    }

    #[tokio::test]
    async fn a_resolution_closes_and_requeues_in_one_transaction() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1), exec(0), exec(1)])
            .into_connection();

        let resolved = resolve_cluster_attempt(
            &db,
            ClusterJobId::now_v7(),
            ClusterAttemptId::now_v7(),
            ClusterAttemptOutcome::Failed,
            true,
            ClusterJobStatus::Failed,
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, Some(true));
        let statements = logged(db);
        assert_eq!(statements.first().map(|s| s.sql.as_str()), Some("BEGIN"));
        assert_eq!(statements.last().map(|s| s.sql.as_str()), Some("COMMIT"));
        let cluster_updates = statements
            .iter()
            .filter(|s| s.sql.starts_with("UPDATE \"cluster_job\""))
            .count();
        assert_eq!(
            cluster_updates, 1,
            "a requeued cluster is not also finished"
        );
    }

    #[tokio::test]
    async fn resolving_a_closed_attempt_writes_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0)])
            .into_connection();

        let resolved = resolve_cluster_attempt(
            &db,
            ClusterJobId::now_v7(),
            ClusterAttemptId::now_v7(),
            ClusterAttemptOutcome::Failed,
            true,
            ClusterJobStatus::Failed,
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, None);
        assert!(logged(db).iter().any(|s| s.sql == "ROLLBACK"));
    }

    #[tokio::test]
    async fn a_failed_prepare_returns_a_started_cluster_to_the_queue() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1), exec(0), exec(1)])
            .into_connection();

        assert!(
            fail_prepare_attempt(&db, ClusterJobId::now_v7(), ClusterAttemptId::now_v7())
                .await
                .expect("fail")
        );

        let statements = logged(db);
        let reset = statements
            .iter()
            .find(|s| s.sql.starts_with("UPDATE \"cluster_job\""))
            .expect("status reset");
        let values = format!("{:?}", reset.values);
        let queued = format!("SmallInt(Some({}))", i16::from(ClusterJobStatus::Queued));
        let running = format!("SmallInt(Some({}))", i16::from(ClusterJobStatus::Running));
        assert!(
            values.contains(&queued) && values.contains(&running),
            "{values}"
        );
    }
}
