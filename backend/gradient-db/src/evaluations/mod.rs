/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod counters;
pub mod draining;
pub mod expected_thunks;
pub mod failed_attributes;
pub mod imports;
pub mod watchdog;

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect,
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
