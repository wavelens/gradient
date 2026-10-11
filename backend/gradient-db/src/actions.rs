/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    Order, QueryFilter, QueryOrder, QuerySelect,
};

pub async fn of_task<C: ConnectionTrait>(db: &C, task: TaskId) -> Result<Vec<MTaskAction>, DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Task.eq(task))
        .all(db)
        .await
}

pub async fn by_id_in_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    id: TaskActionId,
) -> Result<Option<MTaskAction>, DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(task))
        .one(db)
        .await
}

pub async fn by_name<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    name: &str,
) -> Result<Option<MTaskAction>, DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Task.eq(task))
        .filter(CTaskAction::Name.eq(name))
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(db: &C, action: MTaskAction) -> Result<MTaskAction, DbErr> {
    action.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(
    db: &C,
    action: ATaskAction,
    read_at: NaiveDateTime,
) -> Result<Option<MTaskAction>, DbErr> {
    crate::rows::update_where::<ETaskAction, _>(
        db,
        action,
        Condition::all().add(CTaskAction::UpdatedAt.eq(read_at)),
    )
    .await
}

pub async fn delete<C: ConnectionTrait>(db: &C, action: MTaskAction) -> Result<(), DbErr> {
    action.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn deliveries_of<C: ConnectionTrait>(
    db: &C,
    action: TaskActionId,
    limit: u64,
    offset: u64,
) -> Result<Vec<MTaskActionDelivery>, DbErr> {
    ETaskActionDelivery::find()
        .filter(CTaskActionDelivery::ActionId.eq(action))
        .order_by(CTaskActionDelivery::DeliveredAt, Order::Desc)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await
}

pub async fn delivery_of<C: ConnectionTrait>(
    db: &C,
    action: TaskActionId,
    id: TaskActionDeliveryId,
) -> Result<Option<MTaskActionDelivery>, DbErr> {
    ETaskActionDelivery::find_by_id(id)
        .filter(CTaskActionDelivery::ActionId.eq(action))
        .one(db)
        .await
}
