/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Organisation ↔ cache subscription helpers consumed by the trigger
//! pipeline (no-cache gate) and the cache-create reconcile path.

use gradient_types::ids::OrganizationId;
use gradient_entity::organization_cache::CacheSubscriptionMode;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

/// Returns `true` when the organisation has at least one active cache
/// subscription that can receive build outputs (ReadWrite or WriteOnly).
///
/// ReadOnly subscriptions are excluded - they let the org pull but not push,
/// so a build would have nowhere to land.
pub async fn org_has_writable_cache<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<bool, sea_orm::DbErr> {
    use gradient_entity::cache::{Column as CCache, Entity as ECache};
    use gradient_entity::ids::CacheId;
    use gradient_entity::organization_cache::{Column as COC, Entity as EOC};

    let cache_ids: Vec<CacheId> = EOC::find()
        .filter(COC::Organization.eq(organization))
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
