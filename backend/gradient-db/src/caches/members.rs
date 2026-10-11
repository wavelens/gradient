/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter};

pub async fn count_with_role<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    role: RoleId,
) -> Result<u64, DbErr> {
    ECacheUser::find()
        .filter(CCacheUser::Cache.eq(cache))
        .filter(CCacheUser::Role.eq(role))
        .count(db)
        .await
}
