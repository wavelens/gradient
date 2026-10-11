/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, IntoActiveModel, JsonValue,
};

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    session: MUploadSession,
) -> Result<MUploadSession, DbErr> {
    session.into_active_model().insert(db).await
}

pub async fn set_missing<C: ConnectionTrait>(
    db: &C,
    session: MUploadSession,
    missing: JsonValue,
) -> Result<MUploadSession, DbErr> {
    let mut active: AUploadSession = session.into();
    active.missing = Set(missing);
    active.update(db).await
}

pub async fn mark_dispatched<C: ConnectionTrait>(
    db: &C,
    session: MUploadSession,
    at: NaiveDateTime,
) -> Result<Option<MUploadSession>, DbErr> {
    let mut active: AUploadSession = session.into();
    active.dispatched_at = Set(Some(at));
    crate::rows::update_where::<EUploadSession, _>(
        db,
        active,
        Condition::all().add(CUploadSession::DispatchedAt.is_null()),
    )
    .await
}
