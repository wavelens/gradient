/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn with_hash<C: ConnectionTrait>(db: &C, hash: Vec<u8>) -> Result<Vec<MCommit>, DbErr> {
    ECommit::find().filter(CCommit::Hash.eq(hash)).all(db).await
}
