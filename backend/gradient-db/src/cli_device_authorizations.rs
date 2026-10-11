/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
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

fn still_pending(at: NaiveDateTime) -> Condition {
    Condition::all()
        .add(CCliDeviceAuthorization::AuthorizedAt.is_null())
        .add(CCliDeviceAuthorization::DeniedAt.is_null())
        .add(CCliDeviceAuthorization::ExpiresAt.gte(at))
}

pub async fn clear_token<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    let mut active = authorization.into_active_model();
    active.token = Set(None);
    crate::rows::update_where::<ECliDeviceAuthorization, _>(
        db,
        active,
        Condition::all().add(CCliDeviceAuthorization::Token.is_not_null()),
    )
    .await
}

pub async fn authorize<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
    user: UserId,
    token: String,
    at: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    let mut active = authorization.into_active_model();
    active.user_id = Set(Some(user));
    active.token = Set(Some(token));
    active.authorized_at = Set(Some(at));
    crate::rows::update_where::<ECliDeviceAuthorization, _>(db, active, still_pending(at)).await
}

pub async fn deny<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
    at: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    let mut active = authorization.into_active_model();
    active.denied_at = Set(Some(at));
    crate::rows::update_where::<ECliDeviceAuthorization, _>(db, active, still_pending(at)).await
}
