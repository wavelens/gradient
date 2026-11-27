/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::StorePath;
use gradient_entity::derivation::Column as CDerivation;
use gradient_entity::dispatched_job::{Column as CDispatchedJob, Entity as EDispatchedJob};
use gradient_entity::entry_point::{Column as CEntryPoint, Entity as EEntryPoint};
use gradient_entity::eval_shard::{
    ActiveModel as AEvalShard, Column as CEvalShard, Entity as EEvalShard, EvalShardState,
    Model as MEvalShard,
};
use gradient_entity::eval_split::{Column as CEvalSplit, Entity as EEvalSplit};
use gradient_entity::ids::{EvalShardId, EvaluationId};
use sea_orm::sea_query::{Expr, LockType, Query};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, ExprTrait,
    FromQueryResult, IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, RelationTrait,
    TransactionTrait, Value,
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

pub async fn shard_drv_paths<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    pattern: &str,
    children: Option<&[String]>,
) -> Result<Vec<String>, DbErr> {
    let set: Vec<&str> = pattern
        .split('.')
        .take_while(|segment| !segment.contains('*'))
        .collect();
    let set = set.join(".");
    let attrs: Vec<String> = match children {
        Some(children) => children.iter().map(|c| format!("{set}.{c}")).collect(),
        None => vec![set],
    };
    let under = attrs.iter().fold(Condition::any(), |any, attr| {
        any.add(CEntryPoint::Eval.eq(attr.as_str()))
            .add(CEntryPoint::Eval.like(format!("{}.%", escape_like(attr))))
    });
    let rows: Vec<(String, String)> = EEntryPoint::find()
        .select_only()
        .column(CDerivation::Hash)
        .column(CDerivation::Name)
        .join(
            sea_orm::JoinType::InnerJoin,
            gradient_entity::entry_point::Relation::Derivation.def(),
        )
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .filter(under)
        .into_tuple()
        .all(db)
        .await?;

    Ok(rows
        .into_iter()
        .map(|(hash, name)| {
            StorePath::from_hash_and_name_without_validation(hash, format!("{name}.drv")).full()
        })
        .collect())
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub async fn requeue_shards_of<C>(
    db: &C,
    job: &str,
    held: &[EvalShardId],
) -> Result<Vec<FailedShard>, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    requeue_or_fail_running_shards_matching_and_collect_failed(
        db,
        Condition::all()
            .add(CEvalShard::TakenJob.eq(job))
            .add(CEvalShard::Id.is_not_in(held.iter().copied())),
    )
    .await
}

pub async fn requeue_running_shards_whose_job_has_no_open_assignment<C>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<FailedShard>, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let open_assignment = Query::select()
        .expr(Expr::val(1))
        .from(EDispatchedJob)
        .and_where(
            Expr::col((EDispatchedJob, CDispatchedJob::JobId))
                .equals((EEvalShard, CEvalShard::TakenJob)),
        )
        .and_where(Expr::col((EDispatchedJob, CDispatchedJob::FinishedAt)).is_null())
        .to_owned();

    requeue_or_fail_running_shards_matching_and_collect_failed(
        db,
        Condition::all()
            .add(CEvalShard::Evaluation.eq(evaluation))
            .add(Expr::exists(open_assignment).not()),
    )
    .await
}

async fn requeue_or_fail_running_shards_matching_and_collect_failed<C>(
    db: &C,
    matching: Condition,
) -> Result<Vec<FailedShard>, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    let running = EEvalShard::find()
        .filter(matching)
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

    fn new_shard(children: &[&str]) -> NewShard {
        NewShard {
            pattern: "packages.*".into(),
            children: Some(children.iter().map(|c| c.to_string()).collect()),
            predicted_ms: 10.0,
        }
    }

    #[tokio::test]
    async fn test_shard_queue_take_order_lease_finish_by_wrong_job_requeue_held_shards_split_on_loss_and_fail_on_second_loss()
     {
        let evaluation = EvaluationId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(evaluation)
            .into_database()
            .await;
        let conn = db.connection();
        insert_shards(
            &conn,
            evaluation,
            &[new_shard(&["x"]), new_shard(&["y", "z", "w"])],
            None,
        )
        .await
        .unwrap();
        insert_shards(&conn, evaluation, &[new_shard(&["m"])], Some("job-B"))
            .await
            .unwrap();

        let seq_of = |shard: Option<MEvalShard>| shard.map(|s| s.seq);
        let first = seq_of(take_shard(&conn, evaluation, "job-A").await.unwrap());
        let own_reported = seq_of(take_shard(&conn, evaluation, "job-B").await.unwrap());
        let second = take_shard(&conn, evaluation, "job-A").await.unwrap();
        let drained = take_shard(&conn, evaluation, "job-A").await.unwrap();
        let by_seq = |seq: i32| {
            let db = &db;
            async move {
                db.rows::<EEvalShard>()
                    .await
                    .into_iter()
                    .find(|s| s.seq == seq)
                    .expect("the shard exists")
            }
        };
        let (split_id, single_id) = (by_seq(2).await.id, by_seq(1).await.id);

        let by_wrong_job = finish_shard(&conn, single_id, "job-B", &[new_shard(&["late"])])
            .await
            .unwrap();
        let shards_after_wrong_job = db.rows::<EEvalShard>().await.len();
        let by_owner = finish_shard(&conn, single_id, "job-A", &[new_shard(&["late"])])
            .await
            .unwrap();
        let held = requeue_shards_of(&conn, "job-A", &[split_id])
            .await
            .unwrap();
        let held_state = by_seq(2).await.state;
        let split_failures = requeue_shards_of(&conn, "job-A", &[]).await.unwrap();
        let single_failures = requeue_shards_of(&conn, "job-B", &[]).await.unwrap();
        let requeued_attempts = (by_seq(3).await.attempts, by_seq(3).await.state);
        let retaken = seq_of(take_shard(&conn, evaluation, "job-B").await.unwrap());
        let failed = requeue_shards_of(&conn, "job-B", &[]).await.unwrap();

        let mut shards = db.rows::<EEvalShard>().await;
        shards.sort_by_key(|s| s.seq);
        let summary: Vec<_> = shards
            .iter()
            .map(|s| (s.seq, s.state, s.attempts, s.children.clone().unwrap()))
            .collect();
        let late = shards.iter().find(|s| s.seq == 4).unwrap();
        db.finish().await;

        assert_eq!((first, own_reported), (Some(1), Some(3)));
        assert_eq!(second.map(|s| s.seq), Some(2));
        assert!(drained.is_none());
        assert!(!by_wrong_job && by_owner);
        assert_eq!(shards_after_wrong_job, 3);
        assert_eq!(late.reported_by_job.as_deref(), Some("job-A"));
        assert!(held.is_empty() && held_state == EvalShardState::Running);
        assert!(split_failures.is_empty() && single_failures.is_empty());
        assert_eq!(
            retaken,
            Some(3),
            "the reporting job gets its own shard first"
        );
        assert_eq!(requeued_attempts, (1, EvalShardState::Queued));
        assert_eq!(
            failed,
            vec![FailedShard {
                evaluation,
                pattern: "packages.*".into(),
                children: Some(vec!["m".into()]),
            }]
        );
        let strings = |items: &[&str]| items.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                (1, EvalShardState::Done, 0, strings(&["x"])),
                (2, EvalShardState::Split, 1, strings(&["y", "z", "w"])),
                (3, EvalShardState::Failed, 2, strings(&["m"])),
                (4, EvalShardState::Queued, 0, strings(&["late"])),
                (5, EvalShardState::Queued, 1, strings(&["y"])),
                (6, EvalShardState::Queued, 1, strings(&["z", "w"])),
            ]
        );
    }

    #[tokio::test]
    async fn only_a_shard_whose_job_lost_its_assignment_is_queued_again() {
        let evaluation = EvaluationId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(evaluation)
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv")
            .job()
            .open_assignment()
            .assignment_option(|row| row.job_id = Some("eval:alive".into()))
            .into_database()
            .await;
        let conn = db.connection();
        let taken_by = |seq: i32, job: &str| MEvalShard {
            taken_job: Some(job.into()),
            ..shard(evaluation, seq, &["only"])
        };
        let (alive, lost) = (taken_by(1, "eval:alive"), taken_by(2, "eval:lost"));
        EEvalShard::insert_many([
            alive.clone().into_active_model(),
            lost.clone().into_active_model(),
        ])
        .exec(&conn)
        .await
        .expect("the shards insert");

        let failed = requeue_running_shards_whose_job_has_no_open_assignment(&conn, evaluation)
            .await
            .expect("requeue");

        assert!(failed.is_empty());
        let alive_row = db
            .find::<EEvalShard>(alive.id)
            .await
            .expect("the alive shard");
        let lost_row = db
            .find::<EEvalShard>(lost.id)
            .await
            .expect("the lost shard");
        assert_eq!(
            (alive_row.state, alive_row.taken_job.as_deref()),
            (EvalShardState::Running, Some("eval:alive"))
        );
        assert_eq!(
            (lost_row.state, lost_row.taken_job),
            (EvalShardState::Queued, None)
        );
        db.finish().await;
    }
}
