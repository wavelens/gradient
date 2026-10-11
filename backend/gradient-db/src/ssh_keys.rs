/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub async fn of_user<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Vec<MUserSshKey>, DbErr> {
    EUserSshKey::find()
        .filter(CUserSshKey::User.eq(user))
        .order_by_desc(CUserSshKey::CreatedAt)
        .all(db)
        .await
}

pub async fn by_fingerprint<C: ConnectionTrait>(
    db: &C,
    fingerprint: &str,
) -> Result<Option<MUserSshKey>, DbErr> {
    EUserSshKey::find()
        .filter(CUserSshKey::Fingerprint.eq(fingerprint))
        .one(db)
        .await
}
