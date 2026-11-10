/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, IntoActiveModel, Value,
};

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    session: MUploadSession,
) -> Result<MUploadSession, DbErr> {
    session.into_active_model().insert(db).await
}

crate::sql! {
    DECREASE_UPLOAD_SESSION_MISSING_COUNT_RETURNING_REMAINING = "UPDATE upload_session \
         SET missing = missing - $1 \
         WHERE id = $2 \
         RETURNING jsonb_array_length(missing)::bigint AS remaining",
        params = [Text("0"), NewUuid];
}

pub async fn remove_missing_blob_hash_and_count_remaining<C: ConnectionTrait>(
    db: &C,
    session: UploadSessionId,
    blob_hash: &str,
) -> Result<i64, DbErr> {
    DECREASE_UPLOAD_SESSION_MISSING_COUNT_RETURNING_REMAINING
        .scalar(
            db,
            [
                Value::String(Some(blob_hash.to_owned())),
                Value::Uuid(Some(session.into_inner())),
            ],
            "remaining",
        )
        .await
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
