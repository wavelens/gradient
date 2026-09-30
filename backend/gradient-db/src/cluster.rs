/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster attempts: all members of an attempt are claimed, or none is.

use crate::dispatch_record::{ClaimGate, abandon_open, claim_statement};
use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::cluster_attempt::{
    ClusterAttemptOutcome, Column as CClusterAttempt, Entity as EClusterAttempt,
};
use gradient_entity::cluster_job::Model as MClusterJob;
use gradient_entity::cluster_job::{
    ClusterJobStatus, Column as CClusterJob, Entity as EClusterJob,
};
use gradient_entity::cluster_member::Model as MClusterMember;
use gradient_entity::cluster_member::{Column as CClusterMember, Entity as EClusterMember};
use gradient_entity::derivation_build::Entity as EDerivationBuild;
use gradient_entity::dispatched_job::{Column as CDispatchedJob, Model as MDispatchedJob};
use gradient_entity::evaluation::{Entity as EEvaluation, EvaluationStatus};
use gradient_entity::ids::{
    ClusterAttemptId, ClusterJobId, ClusterMemberId, DerivationBuildId, EvaluationId,
};
use gradient_types::{CDerivationBuild, CEvaluation};
use sea_orm::sea_query::{Cond, Expr, InsertStatement, OnConflict, Query, SelectStatement};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, ExprTrait,
    FromQueryResult, QueryFilter, TransactionTrait, Value,
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

/// Stamp `started_at` on `attempt` while it is open and unstarted, and only then
/// set its cluster `Running` and count the attempt; `false` when nothing moved.
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

crate::sql! {
    MEMBERSHIP = r#"
SELECT m.id AS member_id, m.cluster_job, m.evaluation, m.derivation_build, m.role, m."primary", m.pin,
       c.status, c.same_zone, c.attempts, c.retry_budget, c.created_at, c.updated_at,
       (SELECT count(*) FROM cluster_member s WHERE s.cluster_job = m.cluster_job) AS member_count
FROM cluster_member m
JOIN cluster_job c ON c.id = m.cluster_job
WHERE m."evaluation" = ANY($1::uuid[]) OR m."derivation_build" = ANY($2::uuid[])"#,
        params = [EvaluationIds(8), AnchorIds(8)];
}

#[derive(Debug, Clone)]
pub struct MemberOf {
    pub cluster: MClusterJob,
    pub member: MClusterMember,
    pub member_count: u32,
}

#[derive(FromQueryResult)]
struct MemberRow {
    member_id: ClusterMemberId,
    cluster_job: ClusterJobId,
    evaluation: Option<EvaluationId>,
    derivation_build: Option<DerivationBuildId>,
    role: String,
    primary: bool,
    pin: Option<String>,
    status: ClusterJobStatus,
    same_zone: bool,
    attempts: i32,
    retry_budget: i32,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
    member_count: i64,
}

impl From<MemberRow> for MemberOf {
    fn from(r: MemberRow) -> Self {
        Self {
            cluster: MClusterJob {
                id: r.cluster_job,
                status: r.status,
                same_zone: r.same_zone,
                attempts: r.attempts,
                retry_budget: r.retry_budget,
                created_at: r.created_at,
                updated_at: r.updated_at,
            },
            member: MClusterMember {
                id: r.member_id,
                cluster_job: r.cluster_job,
                evaluation: r.evaluation,
                derivation_build: r.derivation_build,
                role: r.role,
                primary: r.primary,
                pin: r.pin,
            },
            member_count: u32::try_from(r.member_count).unwrap_or(u32::MAX),
        }
    }
}

/// The cluster memberships of the named jobs, one row per member with its
/// cluster and the cluster's member count.
pub async fn cluster_membership<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
    anchors: &[DerivationBuildId],
) -> Result<Vec<MemberOf>, DbErr> {
    if evaluations.is_empty() && anchors.is_empty() {
        return Ok(Vec::new());
    }

    let evaluations: Vec<uuid::Uuid> = evaluations.iter().map(|e| e.into_inner()).collect();
    let anchors: Vec<uuid::Uuid> = anchors.iter().map(|a| a.into_inner()).collect();
    let rows = MemberRow::find_by_statement(
        MEMBERSHIP.bind([Value::from(evaluations), Value::from(anchors)]),
    )
    .all(db)
    .await?;

    Ok(rows.into_iter().map(MemberOf::from).collect())
}

/// A member of the correlated `cluster_job` whose job can no longer run: a
/// terminal evaluation, or an anchor that is done or failed for good.
pub(crate) fn dead_member_of_cluster() -> SelectStatement {
    let dead_evaluation = Query::select()
        .expr(Expr::val(1))
        .from(EEvaluation)
        .and_where(
            Expr::col((EEvaluation, CEvaluation::Id))
                .equals((EClusterMember, CClusterMember::Evaluation)),
        )
        .and_where(
            Expr::col((EEvaluation, CEvaluation::Status))
                .is_in(EvaluationStatus::TERMINAL.map(|s| s.into_value())),
        )
        .to_owned();
    let dead_anchor = Query::select()
        .expr(Expr::val(1))
        .from(EDerivationBuild)
        .and_where(
            Expr::col((EDerivationBuild, CDerivationBuild::Id))
                .equals((EClusterMember, CClusterMember::DerivationBuild)),
        )
        .and_where(
            Expr::col((EDerivationBuild, CDerivationBuild::Status)).is_in(
                BuildStatus::TERMINAL_SUCCESS
                    .into_iter()
                    .chain(BuildStatus::REQUEUEABLE)
                    .map(|s| s.into_value()),
            ),
        )
        .to_owned();

    Query::select()
        .expr(Expr::val(1))
        .from(EClusterMember)
        .and_where(
            Expr::col((EClusterMember, CClusterMember::ClusterJob))
                .equals((EClusterJob, CClusterJob::Id)),
        )
        .cond_where(
            Cond::any()
                .add(Expr::exists(dead_evaluation))
                .add(Expr::exists(dead_anchor)),
        )
        .to_owned()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use gradient_entity::ids::{ClusterMemberId, DerivationBuildId, DispatchedJobId, EvaluationId};
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

    fn membership_row(
        member: ClusterMemberId,
        cluster: ClusterJobId,
        evaluation: EvaluationId,
        count: i64,
    ) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        let now = chrono::Utc::now().naive_utc();
        std::collections::BTreeMap::from([
            ("member_id", member.into_inner().into()),
            ("cluster_job", cluster.into_inner().into()),
            ("evaluation", Some(evaluation.into_inner()).into()),
            ("derivation_build", Option::<uuid::Uuid>::None.into()),
            ("role", "server".into()),
            ("primary", true.into()),
            ("pin", Option::<String>::None.into()),
            ("status", i16::from(ClusterJobStatus::Queued).into()),
            ("same_zone", true.into()),
            ("attempts", 0i32.into()),
            ("retry_budget", 2i32.into()),
            ("created_at", now.into()),
            ("updated_at", now.into()),
            ("member_count", count.into()),
        ])
    }

    #[tokio::test]
    async fn membership_maps_each_member_to_its_cluster_and_size() {
        let (member, cluster, evaluation) = (
            ClusterMemberId::now_v7(),
            ClusterJobId::now_v7(),
            EvaluationId::now_v7(),
        );
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![membership_row(member, cluster, evaluation, 3)]])
            .into_connection();

        let found = cluster_membership(&db, &[evaluation], &[])
            .await
            .expect("membership");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].member.id, member);
        assert_eq!(found[0].member.evaluation, Some(evaluation));
        assert_eq!(found[0].cluster.id, cluster);
        assert!(found[0].cluster.same_zone);
        assert_eq!(found[0].member_count, 3);
        let sql = &logged(db)[0].sql;
        assert!(sql.contains("\"evaluation\" = ANY($1::uuid[])"), "{sql}");
        assert!(
            sql.contains("\"derivation_build\" = ANY($2::uuid[])"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn membership_of_nothing_asks_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let found = cluster_membership(&db, &[], &[]).await.expect("membership");

        assert!(found.is_empty());
        assert!(logged(db).is_empty());
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
}
