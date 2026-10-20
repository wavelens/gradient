/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    blob: MBuildRequestBlob,
) -> Result<MBuildRequestBlob, DbErr> {
    blob.into_active_model().insert(db).await
}

pub async fn mark_stored_as_used<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    hashes: Vec<Vec<u8>>,
    at: NaiveDateTime,
) -> Result<Vec<Vec<u8>>, DbErr> {
    let stored = EBuildRequestBlob::update_many()
        .col_expr(CBuildRequestBlob::LastUsedAt, Expr::value(at))
        .filter(CBuildRequestBlob::Project.eq(project))
        .filter(CBuildRequestBlob::Hash.is_in(hashes))
        .exec_with_returning(db)
        .await?;

    Ok(stored.into_iter().map(|blob| blob.hash).collect())
}
