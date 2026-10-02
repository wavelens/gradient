/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::ids::ProjectId;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub async fn project_has_writable_cache<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<bool, sea_orm::DbErr> {
    use gradient_entity::cache::{Column as CCache, Entity as ECache};
    use gradient_entity::ids::CacheId;
    use gradient_entity::project_cache::{Column as COC, Entity as EOC};

    let cache_ids: Vec<CacheId> = EOC::find()
        .filter(COC::Project.eq(project))
        .filter(COC::Mode.is_in([
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::WriteOnly,
        ]))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.cache)
        .collect();

    if cache_ids.is_empty() {
        return Ok(false);
    }

    let row = ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .filter(CCache::Active.eq(true))
        .one(db)
        .await?;
    Ok(row.is_some())
}
