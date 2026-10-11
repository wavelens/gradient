/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Value};

fn owned_or_builtin<C: ColumnTrait>(owner_column: C, owner: impl Into<Value>) -> Condition {
    Condition::any()
        .add(owner_column.eq(owner))
        .add(owner_column.is_null())
}

pub async fn builtin_or_owned<E, C>(
    db: &C,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<Vec<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find()
        .filter(
            Condition::any()
                .add(owner_column.is_null())
                .add(owner_column.eq(owner)),
        )
        .all(db)
        .await
}

pub async fn by_name<E, C>(
    db: &C,
    name_column: E::Column,
    name: &str,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<Option<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    E::find()
        .filter(
            Condition::all()
                .add(name_column.eq(name))
                .add(owned_or_builtin(owner_column, owner)),
        )
        .one(db)
        .await
}

pub async fn name_taken<E, C>(
    db: &C,
    name_column: E::Column,
    name: &str,
    id_column: E::Column,
    except: Option<RoleId>,
    owner_column: E::Column,
    owner: impl Into<Value>,
) -> Result<bool, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    let mut query = E::find().filter(name_column.eq(name));
    if let Some(role_id) = except {
        query = query.filter(id_column.ne(role_id));
    }
    let clash = query
        .filter(owned_or_builtin(owner_column, owner))
        .one(db)
        .await?;
    Ok(clash.is_some())
}
