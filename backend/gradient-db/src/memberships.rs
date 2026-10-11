/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter,
    QuerySelect, RelationDef, Value,
};

pub async fn by_owner_and_user<E, C>(
    db: &C,
    owner_column: E::Column,
    owner: impl Into<Value>,
    user_column: E::Column,
    user: UserId,
) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find()
        .filter(
            Condition::all()
                .add(owner_column.eq(owner))
                .add(user_column.eq(user)),
        )
        .one(db)
        .await
}

pub async fn with_users<E, C>(
    db: &C,
    user_relation: RelationDef,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<Vec<(E::Model, Option<MUser>)>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find()
        .join(JoinType::InnerJoin, user_relation)
        .select_also(gradient_entity::user::Entity)
        .filter(owner_column.eq(owner))
        .all(db)
        .await
}

pub async fn role_assigned<E, C>(
    db: &C,
    role_column: E::Column,
    role: RoleId,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<bool, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    Ok(E::find()
        .filter(role_column.eq(role))
        .filter(owner_column.eq(owner))
        .one(db)
        .await?
        .is_some())
}
