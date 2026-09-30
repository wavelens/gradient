/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster attempts: all members of an attempt are claimed, or none is.

use crate::dispatch_record::{ClaimGate, abandon_open, claim_statement};
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

/// Close `attempt` if still open, abandoning its open member rows with it;
/// `false` when another closer got there first and nothing was touched.
pub async fn close_cluster_attempt<C>(
    db: &C,
    attempt: ClusterAttemptId,
    outcome: ClusterAttemptOutcome,
) -> Result<bool, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    let closed = EClusterAttempt::update_many()
        .col_expr(
            CClusterAttempt::FinishedAt,
            Expr::value(gradient_types::now()),
        )
        .col_expr(CClusterAttempt::Outcome, Expr::value(i16::from(outcome)))
        .filter(CClusterAttempt::Id.eq(attempt))
        .filter(CClusterAttempt::FinishedAt.is_null())
        .exec(&txn)
        .await?
        .rows_affected
        == 1;
    if closed {
        abandon_open(
            &txn,
            Some(Expr::col(CDispatchedJob::ClusterAttempt).eq(attempt)),
        )
        .await?;
    }
    txn.commit().await?;

    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use gradient_entity::ids::{DerivationBuildId, DispatchedJobId, EvaluationId};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement};

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

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
                        anchor: DerivationBuildId::now_v7(),
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

    fn logged(db: sea_orm::DatabaseConnection) -> Vec<Statement> {
        db.into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .collect()
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
}
