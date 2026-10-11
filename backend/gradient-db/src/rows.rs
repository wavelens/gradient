/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::IntoActiveModel;
use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, Condition, ConnectionTrait, DbErr, EntityTrait,
    QueryFilter,
};

pub async fn insert<E, C>(db: &C, row: E::Model) -> Result<E::Model, DbErr>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: ActiveModelBehavior + Send,
    C: ConnectionTrait,
{
    row.into_active_model().insert(db).await
}

pub async fn update<E, C>(db: &C, row: E::ActiveModel) -> Result<E::Model, DbErr>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: ActiveModelBehavior + Send,
    C: ConnectionTrait,
{
    row.update(db).await
}

pub async fn update_where<E, C>(
    db: &C,
    row: E::ActiveModel,
    unchanged: Condition,
) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: ActiveModelBehavior + Send,
    C: ConnectionTrait,
{
    match E::update(row).validate()?.filter(unchanged).exec(db).await {
        Ok(model) => Ok(Some(model)),
        Err(DbErr::RecordNotUpdated) => Ok(None),
        Err(err) => Err(err),
    }
}

pub async fn delete<E, C>(db: &C, row: E::Model) -> Result<(), DbErr>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: ActiveModelBehavior + Send,
    C: ConnectionTrait,
{
    row.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn delete_was_present<E, C>(db: &C, row: E::Model) -> Result<bool, DbErr>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: ActiveModelBehavior + Send,
    C: ConnectionTrait,
{
    let deleted = row.into_active_model().delete(db).await?;
    Ok(deleted.rows_affected > 0)
}
