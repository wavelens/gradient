/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::eval_shard::{
    ActiveModel as AEvalShard, Column as CEvalShard, Entity as EEvalShard, EvalShardState,
    Model as MEvalShard,
};
use gradient_entity::eval_split::{Column as CEvalSplit, Entity as EEvalSplit};
use gradient_entity::ids::{EvalShardId, EvaluationId};
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult,
    IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, TransactionTrait, Value,
};

use crate::chunked::insert_in_chunks;

pub const MAX_SHARD_ATTEMPTS: i16 = 2;
const SHARDS_PER_STATEMENT: usize = 1000;

crate::sql! {
    EVAL_SHARD_TAKE = "UPDATE eval_shard SET state = 1, taken_job = $2, taken_at = (now() AT TIME ZONE 'UTC') \
         WHERE id = ( \
             SELECT id FROM eval_shard WHERE evaluation = $1 AND state = 0 \
             ORDER BY CASE WHEN reported_by_job = $2 THEN 0 WHEN reported_by_job IS NULL THEN 1 ELSE 2 END, seq \
             LIMIT 1 FOR UPDATE SKIP LOCKED) \
         RETURNING id, evaluation, seq, pattern, children, predicted_ms, state, attempts, \
             reported_by_job, taken_job, created_at, taken_at, finished_at",
        params = [EvaluationId, Text("eval:probe")];
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewShard {
    pub pattern: String,
    pub children: Option<Vec<String>>,
    pub predicted_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FailedShard {
    pub evaluation: EvaluationId,
    pub pattern: String,
    pub children: Option<Vec<String>>,
}

#[derive(FromQueryResult)]
struct LastSeq {
    seq: Option<i32>,
}

pub async fn insert_shards<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    shards: &[NewShard],
    reported_by_job: Option<&str>,
) -> Result<(), DbErr> {
    append(db, evaluation, shards, reported_by_job, 0).await
}

pub async fn take_shard<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    job: &str,
) -> Result<Option<MEvalShard>, DbErr> {
    MEvalShard::find_by_statement(EVAL_SHARD_TAKE.bind([
        Value::Uuid(Some(evaluation.into_inner())),
        Value::String(Some(job.to_owned())),
    ]))
    .one(db)
    .await
}

pub async fn finish_shard<C: ConnectionTrait>(
    db: &C,
    shard: EvalShardId,
    job: &str,
    deferred: &[NewShard],
) -> Result<bool, DbErr> {
    let finished = EEvalShard::update_many()
        .col_expr(
            CEvalShard::State,
            Expr::value(i16::from(EvalShardState::Done)),
        )
        .col_expr(CEvalShard::FinishedAt, Expr::value(gradient_types::now()))
        .filter(CEvalShard::Id.eq(shard))
        .filter(CEvalShard::State.eq(EvalShardState::Running))
        .filter(CEvalShard::TakenJob.eq(job))
        .exec_with_returning(db)
        .await?;
    let Some(finished) = finished.into_iter().next() else {
        return Ok(false);
    };

    insert_shards(db, finished.evaluation, deferred, Some(job)).await?;

    Ok(true)
}

pub async fn has_open_shards<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    let open = EEvalShard::find()
        .select_only()
        .column(CEvalShard::Id)
        .filter(CEvalShard::Evaluation.eq(evaluation))
        .filter(CEvalShard::State.is_in([EvalShardState::Queued, EvalShardState::Running]))
        .limit(1)
        .into_tuple::<EvalShardId>()
        .one(db)
        .await?;

    Ok(open.is_some())
}

pub async fn requeue_shards_of<C>(db: &C, job: &str) -> Result<Vec<FailedShard>, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    let running = EEvalShard::find()
        .filter(CEvalShard::TakenJob.eq(job))
        .filter(CEvalShard::State.eq(EvalShardState::Running))
        .order_by_asc(CEvalShard::Seq)
        .lock(LockType::NoKeyUpdate)
        .all(&txn)
        .await?;

    let mut failed = Vec::new();
    for shard in running {
        failed.extend(return_shard(&txn, shard).await?);
    }
    txn.commit().await?;

    Ok(failed)
}

async fn return_shard(
    txn: &DatabaseTransaction,
    shard: MEvalShard,
) -> Result<Option<FailedShard>, DbErr> {
    let attempts = shard.attempts + 1;
    if let Some(children) = shard.children.as_deref().filter(|c| c.len() >= 2) {
        let halves = halves(&shard, children);
        close_shard(txn, shard.id, EvalShardState::Split, attempts).await?;
        append(txn, shard.evaluation, &halves, None, attempts).await?;
        return Ok(None);
    }

    if attempts < MAX_SHARD_ATTEMPTS {
        queue_again(txn, shard.id, attempts).await?;
        return Ok(None);
    }

    close_shard(txn, shard.id, EvalShardState::Failed, attempts).await?;

    Ok(Some(FailedShard {
        evaluation: shard.evaluation,
        pattern: shard.pattern,
        children: shard.children,
    }))
}

fn halves(shard: &MEvalShard, children: &[String]) -> [NewShard; 2] {
    let (first, second) = children.split_at(children.len() / 2);
    let half = |children: &[String]| NewShard {
        pattern: shard.pattern.clone(),
        children: Some(children.to_vec()),
        predicted_ms: shard.predicted_ms / 2.0,
    };

    [half(first), half(second)]
}

async fn append<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    shards: &[NewShard],
    reported_by_job: Option<&str>,
    attempts: i16,
) -> Result<(), DbErr> {
    if shards.is_empty() {
        return Ok(());
    }

    // Appends serialize on the `eval_split` row; the max `seq` is read after the lock, in a fresh snapshot.
    lock_split(db, evaluation).await?;
    let after = last_seq(db, evaluation).await?;
    let now = gradient_types::now();
    let rows: Vec<AEvalShard> = shards
        .iter()
        .zip(after + 1..)
        .map(|(shard, seq)| {
            MEvalShard {
                id: EvalShardId::now_v7(),
                evaluation,
                seq,
                pattern: shard.pattern.clone(),
                children: shard.children.clone(),
                predicted_ms: shard.predicted_ms,
                attempts,
                reported_by_job: reported_by_job.map(str::to_owned),
                created_at: now,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    insert_in_chunks(db, rows, SHARDS_PER_STATEMENT, None).await
}

async fn lock_split<C: ConnectionTrait>(db: &C, evaluation: EvaluationId) -> Result<(), DbErr> {
    EEvalSplit::find()
        .select_only()
        .column(CEvalSplit::Evaluation)
        .filter(CEvalSplit::Evaluation.eq(evaluation))
        .lock(LockType::NoKeyUpdate)
        .into_tuple::<EvaluationId>()
        .one(db)
        .await?;

    Ok(())
}

async fn last_seq<C: ConnectionTrait>(db: &C, evaluation: EvaluationId) -> Result<i32, DbErr> {
    let last = EEvalShard::find()
        .select_only()
        .column_as(CEvalShard::Seq.max(), "seq")
        .filter(CEvalShard::Evaluation.eq(evaluation))
        .into_model::<LastSeq>()
        .one(db)
        .await?;

    Ok(last.and_then(|row| row.seq).unwrap_or(0))
}

async fn close_shard(
    txn: &DatabaseTransaction,
    shard: EvalShardId,
    state: EvalShardState,
    attempts: i16,
) -> Result<(), DbErr> {
    EEvalShard::update_many()
        .col_expr(CEvalShard::State, Expr::value(i16::from(state)))
        .col_expr(CEvalShard::Attempts, Expr::value(attempts))
        .col_expr(CEvalShard::FinishedAt, Expr::value(gradient_types::now()))
        .filter(CEvalShard::Id.eq(shard))
        .exec(txn)
        .await?;

    Ok(())
}

async fn queue_again(
    txn: &DatabaseTransaction,
    shard: EvalShardId,
    attempts: i16,
) -> Result<(), DbErr> {
    EEvalShard::update_many()
        .col_expr(
            CEvalShard::State,
            Expr::value(i16::from(EvalShardState::Queued)),
        )
        .col_expr(CEvalShard::Attempts, Expr::value(attempts))
        .col_expr(CEvalShard::TakenJob, Expr::value(Option::<String>::None))
        .col_expr(
            CEvalShard::TakenAt,
            Expr::value(Option::<chrono::NaiveDateTime>::None),
        )
        .filter(CEvalShard::Id.eq(shard))
        .exec(txn)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};
    use sea_orm::{MockExecResult, Statement};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn shard(evaluation: EvaluationId, seq: i32, children: &[&str]) -> MEvalShard {
        MEvalShard {
            id: EvalShardId::now_v7(),
            evaluation,
            seq,
            pattern: "packages.*".into(),
            children: Some(children.iter().map(|c| c.to_string()).collect()),
            predicted_ms: 1000.0,
            state: EvalShardState::Running,
            taken_job: Some("job-A".into()),
            created_at: gradient_types::now(),
            ..Default::default()
        }
    }

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn split_lock(evaluation: EvaluationId) -> Vec<BTreeMap<&'static str, Value>> {
        vec![BTreeMap::from([(
            "evaluation",
            Value::Uuid(Some(evaluation.into_inner())),
        )])]
    }

    fn last(seq: i32) -> Vec<BTreeMap<&'static str, Value>> {
        vec![BTreeMap::from([("seq", Value::Int(Some(seq)))])]
    }

    fn statement<'a>(statements: &'a [Statement], prefix: &str) -> Vec<&'a Statement> {
        statements
            .iter()
            .filter(|s| s.sql.starts_with(prefix))
            .collect()
    }

    #[tokio::test]
    async fn concurrent_takes_get_different_shards() {
        let evaluation = EvaluationId::now_v7();
        let db = mock_db()
            .append_query_results([
                vec![shard(evaluation, 1, &["a"])],
                vec![shard(evaluation, 2, &["b"])],
            ])
            .into_connection();

        let first = take_shard(&db, evaluation, "job-A").await.expect("take");
        let second = take_shard(&db, evaluation, "job-B").await.expect("take");

        assert_ne!(first.expect("a shard").id, second.expect("a shard").id);
        let statements = all_statements(db.into_transaction_log());
        let sql = norm(&statements[0].sql);
        assert!(sql.starts_with("UPDATE eval_shard SET state = 1"), "{sql}");
        assert!(sql.contains("LIMIT 1 FOR UPDATE SKIP LOCKED"), "{sql}");
    }

    #[tokio::test]
    async fn the_reporting_member_gets_its_deferred_sets_first() {
        let evaluation = EvaluationId::now_v7();
        let mut deferred = shard(evaluation, 2, &["x"]);
        deferred.reported_by_job = Some("job-B".into());
        let db = mock_db()
            .append_query_results([vec![deferred]])
            .into_connection();

        let taken = take_shard(&db, evaluation, "job-B")
            .await
            .expect("take")
            .expect("a shard");

        assert_eq!(taken.seq, 2);
        let statements = all_statements(db.into_transaction_log());
        let sql = norm(&statements[0].sql);
        assert!(
            sql.contains(
                "ORDER BY CASE WHEN reported_by_job = $2 THEN 0 WHEN reported_by_job IS NULL THEN 1 ELSE 2 END, seq"
            ),
            "{sql}"
        );
        assert!(
            format!("{:?}", statements[0].values).contains("job-B"),
            "{:?}",
            statements[0].values
        );
    }

    #[tokio::test]
    async fn finishing_a_shard_of_another_job_changes_nothing() {
        let deferred = [NewShard {
            pattern: "packages.*".into(),
            children: Some(vec!["late".into()]),
            predicted_ms: 10.0,
        }];
        let db = mock_db()
            .append_query_results([Vec::<MEvalShard>::new()])
            .into_connection();

        let finished = finish_shard(&db, EvalShardId::now_v7(), "job-A", &deferred)
            .await
            .expect("finish");

        assert!(!finished);
        let statements = all_statements(db.into_transaction_log());
        assert_eq!(statements.len(), 1, "{statements:?}");
        let sql = &statements[0].sql;
        assert!(sql.contains("\"taken_job\" = $"), "{sql}");
        assert!(statement(&statements, "INSERT").is_empty());
    }

    #[tokio::test]
    async fn a_lost_shard_with_children_is_split_in_halves() {
        let evaluation = EvaluationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![shard(evaluation, 1, &["a", "b", "c"])]])
            .append_query_results([split_lock(evaluation)])
            .append_query_results([last(1)])
            .append_exec_results([exec(1), exec(2)])
            .into_connection();

        let failed = requeue_shards_of(&db, "job-A").await.expect("requeue");

        assert!(failed.is_empty());
        let statements = all_statements(db.into_transaction_log());
        let running = &statement(&statements, "SELECT")[0].sql;
        assert!(running.ends_with("FOR NO KEY UPDATE"), "{running}");
        let closed = format!("{:?}", statement(&statements, "UPDATE")[0].values);
        let split = format!("SmallInt(Some({}))", i16::from(EvalShardState::Split));
        assert!(closed.contains(&split), "{closed}");
        let inserts = statement(&statements, "INSERT INTO \"eval_shard\"");
        assert_eq!(inserts.len(), 1);
        let values = format!("{:?}", inserts[0].values);
        assert!(
            values.contains("[String(Some(\"a\"))]")
                && values.contains("[String(Some(\"b\")), String(Some(\"c\"))]"),
            "{values}"
        );
        assert!(values.contains("SmallInt(Some(1))"), "{values}");
        assert!(values.contains("Int(Some(2))") && values.contains("Int(Some(3))"));
        assert_eq!(statements.last().map(|s| s.sql.as_str()), Some("COMMIT"));
    }

    #[tokio::test]
    async fn a_lost_single_child_shard_fails_on_its_second_loss() {
        let evaluation = EvaluationId::now_v7();
        let mut lost = shard(evaluation, 3, &["hello"]);
        lost.attempts = 1;
        let db = mock_db()
            .append_query_results([vec![lost]])
            .append_exec_results([exec(1)])
            .into_connection();

        let failed = requeue_shards_of(&db, "job-A").await.expect("requeue");

        assert_eq!(
            failed,
            vec![FailedShard {
                evaluation,
                pattern: "packages.*".into(),
                children: Some(vec!["hello".into()]),
            }]
        );
        let statements = all_statements(db.into_transaction_log());
        let values = format!("{:?}", statement(&statements, "UPDATE")[0].values);
        let state = format!("SmallInt(Some({}))", i16::from(EvalShardState::Failed));
        assert!(values.contains(&state), "{values}");
        assert!(statement(&statements, "INSERT").is_empty());
    }
}
