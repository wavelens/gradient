/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    TransactionSession, TransactionTrait,
};

pub async fn of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<Vec<MTaskFlakeInputSource>, DbErr> {
    ETaskFlakeInputSource::find()
        .filter(CTaskFlakeInputSource::Task.eq(task))
        .order_by_asc(CTaskFlakeInputSource::InputName)
        .all(db)
        .await
}

pub async fn replace<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    task: TaskId,
    sources: Vec<MTaskFlakeInputSource>,
) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    ETaskFlakeInputSource::delete_many()
        .filter(CTaskFlakeInputSource::Task.eq(task))
        .exec(&txn)
        .await?;
    if !sources.is_empty() {
        ETaskFlakeInputSource::insert_many(
            sources.into_iter().map(IntoActiveModel::into_active_model),
        )
        .exec_without_returning(&txn)
        .await?;
    }
    txn.commit().await
}
