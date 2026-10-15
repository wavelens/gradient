/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait,
    IntoActiveModel, QueryFilter,
};

pub async fn enable<C: ConnectionTrait>(db: &C, task: TaskId, pr_number: u64) -> Result<(), DbErr> {
    let row = MPullRequestSummary {
        id: PullRequestSummaryId::now_v7(),
        task,
        pr_number: pr_number as i64,
        comment_id: None,
    }
    .into_active_model();
    EPullRequestSummary::insert(row)
        .on_conflict(
            OnConflict::columns([CPullRequestSummary::Task, CPullRequestSummary::PrNumber])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;

    Ok(())
}

pub async fn of_pull_request<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    pr_number: u64,
) -> Result<Option<MPullRequestSummary>, DbErr> {
    EPullRequestSummary::find()
        .filter(CPullRequestSummary::Task.eq(task))
        .filter(CPullRequestSummary::PrNumber.eq(pr_number as i64))
        .one(db)
        .await
}

pub async fn set_comment<C: ConnectionTrait>(
    db: &C,
    id: PullRequestSummaryId,
    comment_id: i64,
) -> Result<(), DbErr> {
    APullRequestSummary {
        id: Set(id),
        comment_id: Set(Some(comment_id)),
        ..Default::default()
    }
    .update(db)
    .await?;

    Ok(())
}
