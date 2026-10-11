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
    QueryFilter,
};

pub async fn of_project_with_hashes<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    hashes: Vec<Vec<u8>>,
) -> Result<Vec<MBuildRequestBlob>, DbErr> {
    EBuildRequestBlob::find()
        .filter(
            Condition::all()
                .add(CBuildRequestBlob::Project.eq(project))
                .add(CBuildRequestBlob::Hash.is_in(hashes)),
        )
        .all(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    blob: MBuildRequestBlob,
) -> Result<MBuildRequestBlob, DbErr> {
    blob.into_active_model().insert(db).await
}

pub async fn mark_used<C: ConnectionTrait>(
    db: &C,
    blob: MBuildRequestBlob,
    at: NaiveDateTime,
) -> Result<MBuildRequestBlob, DbErr> {
    let mut active: ABuildRequestBlob = blob.into();
    active.last_used_at = Set(at);
    active.update(db).await
}
