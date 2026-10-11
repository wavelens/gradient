/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn of_task<C: ConnectionTrait>(db: &C, task: TaskId) -> Result<Vec<MTaskTrigger>, DbErr> {
    ETaskTrigger::find()
        .filter(CTaskTrigger::Task.eq(task))
        .all(db)
        .await
}

pub async fn by_id_in_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    id: TaskTriggerId,
) -> Result<Option<MTaskTrigger>, DbErr> {
    ETaskTrigger::find()
        .filter(CTaskTrigger::Id.eq(id))
        .filter(CTaskTrigger::Task.eq(task))
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    trigger: MTaskTrigger,
) -> Result<MTaskTrigger, DbErr> {
    trigger.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(
    db: &C,
    trigger: ATaskTrigger,
) -> Result<MTaskTrigger, DbErr> {
    trigger.update(db).await
}

pub async fn delete<C: ConnectionTrait>(db: &C, trigger: MTaskTrigger) -> Result<(), DbErr> {
    trigger.into_active_model().delete(db).await?;
    Ok(())
}
