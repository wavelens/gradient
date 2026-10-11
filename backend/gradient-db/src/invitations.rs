/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_user<E, C>(
    db: &C,
    user_column: E::Column,
    user: UserId,
) -> Result<Vec<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find().filter(user_column.eq(user)).all(db).await
}

pub async fn by_token<E, C>(
    db: &C,
    token_column: E::Column,
    token: &str,
) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find().filter(token_column.eq(token)).one(db).await
}
