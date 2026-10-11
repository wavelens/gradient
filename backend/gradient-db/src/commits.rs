/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn with_hash<C: ConnectionTrait>(db: &C, hash: Vec<u8>) -> Result<Vec<MCommit>, DbErr> {
    ECommit::find().filter(CCommit::Hash.eq(hash)).all(db).await
}

pub async fn insert<C: ConnectionTrait>(db: &C, commit: MCommit) -> Result<MCommit, DbErr> {
    commit.into_active_model().insert(db).await
}
