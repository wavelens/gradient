/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder,
};

pub async fn of_user<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Vec<MApi>, DbErr> {
    EApi::find()
        .filter(CApi::OwnedBy.eq(user))
        .order_by_desc(CApi::CreatedAt)
        .all(db)
        .await
}

pub async fn by_owner_and_name<C: ConnectionTrait>(
    db: &C,
    owner: UserId,
    name: &str,
) -> Result<Option<MApi>, DbErr> {
    EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(owner))
                .add(CApi::Name.eq(name)),
        )
        .one(db)
        .await
}

pub async fn other_with_name<C: ConnectionTrait>(
    db: &C,
    owner: UserId,
    name: &str,
    api_key: ApiId,
) -> Result<Option<MApi>, DbErr> {
    EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(owner))
                .add(CApi::Name.eq(name))
                .add(CApi::Id.ne(api_key)),
        )
        .one(db)
        .await
}

pub async fn by_key_hash<C: ConnectionTrait>(
    db: &C,
    key_hash: &str,
) -> Result<Option<MApi>, DbErr> {
    EApi::find().filter(CApi::Key.eq(key_hash)).one(db).await
}

pub async fn stamp_last_used<C: ConnectionTrait>(
    db: &C,
    api_key: MApi,
    at: NaiveDateTime,
) -> Result<(), DbErr> {
    let mut active = api_key.into_active_model();
    active.last_used_at = Set(at);
    active.save(db).await?;
    Ok(())
}

pub async fn revoke<C: ConnectionTrait>(
    db: &C,
    api_key: MApi,
    at: NaiveDateTime,
) -> Result<MApi, DbErr> {
    let mut active = api_key.into_active_model();
    active.revoked_at = Set(Some(at));
    active.update(db).await
}
