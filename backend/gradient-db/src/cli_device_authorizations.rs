/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn unexpired_by_user_code<C: ConnectionTrait>(
    db: &C,
    user_code: &str,
    now: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    ECliDeviceAuthorization::find()
        .filter(CCliDeviceAuthorization::UserCode.eq(user_code))
        .filter(CCliDeviceAuthorization::ExpiresAt.gt(now))
        .one(db)
        .await
}

pub async fn by_device_code_hash<C: ConnectionTrait>(
    db: &C,
    device_code_hash: &str,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    ECliDeviceAuthorization::find()
        .filter(CCliDeviceAuthorization::DeviceCodeHash.eq(device_code_hash))
        .one(db)
        .await
}

pub async fn by_user_code<C: ConnectionTrait>(
    db: &C,
    user_code: &str,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    ECliDeviceAuthorization::find()
        .filter(CCliDeviceAuthorization::UserCode.eq(user_code))
        .one(db)
        .await
}

pub async fn clear_token<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
) -> Result<MCliDeviceAuthorization, DbErr> {
    let mut active = authorization.into_active_model();
    active.token = Set(None);
    active.update(db).await
}

pub async fn authorize<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
    user: UserId,
    token: String,
    at: NaiveDateTime,
) -> Result<MCliDeviceAuthorization, DbErr> {
    let mut active = authorization.into_active_model();
    active.user_id = Set(Some(user));
    active.token = Set(Some(token));
    active.authorized_at = Set(Some(at));
    active.update(db).await
}

pub async fn deny<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
    at: NaiveDateTime,
) -> Result<MCliDeviceAuthorization, DbErr> {
    let mut active = authorization.into_active_model();
    active.denied_at = Set(Some(at));
    active.update(db).await
}
