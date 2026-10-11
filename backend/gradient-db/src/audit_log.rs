/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    Select,
};

pub fn of_user(user: UserId) -> Select<EAuditLog> {
    EAuditLog::find()
        .filter(CAuditLog::UserId.eq(user))
        .order_by_desc(CAuditLog::CreatedAt)
}

pub async fn insert<C: ConnectionTrait>(db: &C, entry: MAuditLog) -> Result<(), DbErr> {
    EAuditLog::insert(entry.into_active_model())
        .exec(db)
        .await?;
    Ok(())
}
