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
    QueryFilter, QueryOrder,
};

pub async fn unrevoked_of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<MSession>, DbErr> {
    ESession::find()
        .filter(CSession::UserId.eq(user))
        .filter(CSession::RevokedAt.is_null())
        .order_by_desc(CSession::LastUsedAt)
        .all(db)
        .await
}

pub async fn revoke<C: ConnectionTrait>(
    db: &C,
    session: MSession,
    at: NaiveDateTime,
) -> Result<MSession, DbErr> {
    let mut active = session.into_active_model();
    active.revoked_at = Set(Some(at));
    active.update(db).await
}

pub async fn stamp_last_used<C: ConnectionTrait>(
    db: &C,
    session: MSession,
    at: NaiveDateTime,
) -> Result<MSession, DbErr> {
    let mut active = session.into_active_model();
    active.last_used_at = Set(at);
    active.update(db).await
}
