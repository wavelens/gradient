/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod branch;
pub mod counters;
pub mod draining;
pub mod expected_thunks;
pub mod failed_attributes;
pub mod flake_input_overrides;
pub mod imports;
pub mod messages;
pub mod watchdog;

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, QuerySelect,
};

pub async fn by_id_in_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    id: EvaluationId,
) -> Result<Option<MEvaluation>, DbErr> {
    EEvaluation::find_by_id(id)
        .filter(CEvaluation::Task.eq(task))
        .one(db)
        .await
}

#[derive(Default)]
pub struct TaskEvaluationFilter {
    pub older_than: Option<MEvaluation>,
    pub commits: Option<Vec<CommitId>>,
    pub statuses: Option<Vec<EvaluationStatus>>,
}

fn older_than(cursor: &MEvaluation) -> Condition {
    Condition::any()
        .add(CEvaluation::CreatedAt.lt(cursor.created_at))
        .add(
            Condition::all()
                .add(CEvaluation::CreatedAt.eq(cursor.created_at))
                .add(CEvaluation::Id.lt(cursor.id)),
        )
}

pub async fn page_of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    filter: TaskEvaluationFilter,
    limit: u64,
) -> Result<Vec<MEvaluation>, DbErr> {
    let mut query = EEvaluation::find().filter(CEvaluation::Task.eq(task));

    if let Some(cursor) = &filter.older_than {
        query = query.filter(older_than(cursor));
    }
    if let Some(commits) = filter.commits {
        query = query.filter(CEvaluation::Commit.is_in(commits));
    }
    if let Some(statuses) = filter.statuses {
        query = query.filter(CEvaluation::Status.is_in(statuses));
    }

    query
        .order_by_desc(CEvaluation::CreatedAt)
        .order_by_desc(CEvaluation::Id)
        .limit(limit)
        .all(db)
        .await
}

pub async fn latest_of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    limit: u64,
) -> Result<Vec<MEvaluation>, DbErr> {
    EEvaluation::find()
        .filter(CEvaluation::Task.eq(task))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(limit)
        .all(db)
        .await
}

pub async fn latest_completed_of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    limit: u64,
) -> Result<Vec<MEvaluation>, DbErr> {
    EEvaluation::find()
        .filter(CEvaluation::Task.eq(task))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Completed))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(limit)
        .all(db)
        .await
}

pub async fn with_ids<C: ConnectionTrait>(
    db: &C,
    ids: &[EvaluationId],
) -> Result<Vec<MEvaluation>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    evaluation: MEvaluation,
) -> Result<MEvaluation, DbErr> {
    evaluation.into_active_model().insert(db).await
}

crate::sql! {
    EVAL_BY_CHECK_RUN_ID = r#"SELECT id FROM evaluation
           WHERE check_run_ids IS NOT NULL
             AND EXISTS (
                 SELECT 1
                 FROM jsonb_each(check_run_ids) AS kv
                 WHERE (kv.value)::text::bigint = $1
             )
           LIMIT 1"#,
        params = [Int(123456)];
}

pub async fn id_by_check_run<C: ConnectionTrait>(
    db: &C,
    check_run: i64,
) -> Result<Option<EvaluationId>, DbErr> {
    // `evaluation.check_run_ids` is a JSON map per check-context name. Any stored id can match the
    // clicked check.
    use sea_orm::FromQueryResult;

    #[derive(FromQueryResult)]
    struct Row {
        id: uuid::Uuid,
    }

    Ok(
        Row::find_by_statement(
            EVAL_BY_CHECK_RUN_ID.bind([sea_orm::Value::BigInt(Some(check_run))]),
        )
        .one(db)
        .await?
        .map(|row| EvaluationId::new(row.id)),
    )
}

pub async fn of_commit<C: ConnectionTrait>(
    db: &C,
    commit: CommitId,
) -> Result<Vec<MEvaluation>, DbErr> {
    EEvaluation::find()
        .filter(CEvaluation::Commit.eq(commit))
        .all(db)
        .await
}

pub async fn repositories<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<Vec<(EvaluationId, String)>, DbErr> {
    crate::fetch_in_chunks(evaluations, |chunk| async move {
        EEvaluation::find()
            .select_only()
            .column(CEvaluation::Id)
            .column(CEvaluation::Repository)
            .filter(CEvaluation::Id.is_in(chunk))
            .into_tuple::<(EvaluationId, String)>()
            .all(db)
            .await
    })
    .await
}
