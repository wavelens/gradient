/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::IntoActiveModel;
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, ConnectionTrait, DbErr, EntityTrait};

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
