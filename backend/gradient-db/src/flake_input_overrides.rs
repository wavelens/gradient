/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder,
};

pub async fn of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<Vec<MTaskFlakeInputOverride>, DbErr> {
    ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Task.eq(task))
        .order_by_asc(CTaskFlakeInputOverride::InputName)
        .all(db)
        .await
}

pub async fn by_id_in_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    id: FlakeInputOverrideId,
) -> Result<Option<MTaskFlakeInputOverride>, DbErr> {
    ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Id.eq(id))
        .filter(CTaskFlakeInputOverride::Task.eq(task))
        .one(db)
        .await
}

pub async fn by_input_name<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    input_name: &str,
) -> Result<Option<MTaskFlakeInputOverride>, DbErr> {
    ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Task.eq(task))
        .filter(CTaskFlakeInputOverride::InputName.eq(input_name))
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    input_override: MTaskFlakeInputOverride,
) -> Result<MTaskFlakeInputOverride, DbErr> {
    input_override.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(
    db: &C,
    input_override: ATaskFlakeInputOverride,
) -> Result<MTaskFlakeInputOverride, DbErr> {
    input_override.update(db).await
}

pub async fn delete<C: ConnectionTrait>(
    db: &C,
    input_override: MTaskFlakeInputOverride,
) -> Result<(), DbErr> {
    input_override.into_active_model().delete(db).await?;
    Ok(())
}
