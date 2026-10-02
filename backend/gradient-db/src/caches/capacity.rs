/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::cache::Model as MCache;
use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::ids::{CacheId, ProjectId};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QuerySelect,
};

pub const STORAGE_HEADROOM_BYTES: i64 = 10 * 1024 * 1024;

/// Postgres is widening `SUM(int8)` to `NUMERIC`.
/// A `NUMERIC` sum is failing to decode into `Option<i64>`.
fn file_size_sum_bigint() -> Expr {
    use gradient_entity::cached_path::Column as CCP;
    use sea_orm::sea_query::ExprTrait;
    CCP::FileSize.sum().cast_as(Alias::new("bigint"))
}

const BYTES_PER_GB: i64 = 1024 * 1024 * 1024;

fn limit_to_bytes(max_storage_gb: i32) -> Option<i64> {
    if max_storage_gb <= 0 {
        None
    } else {
        Some(max_storage_gb as i64 * BYTES_PER_GB)
    }
}

pub async fn cache_used_bytes<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<i64, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::cached_path_signature::{Column as CSig, Entity as ESig};

    let path_ids: Vec<gradient_entity::ids::CachedPathId> = ESig::find()
        .filter(CSig::Cache.eq(cache))
        .all(db)
        .await?
        .into_iter()
        .map(|s| s.cached_path)
        .collect();

    if path_ids.is_empty() {
        return Ok(0);
    }

    let mut total: i64 = 0;
    for chunk in path_ids.chunks(crate::IN_CHUNK_SIZE) {
        let sum: Option<i64> = ECP::find()
            .filter(CCP::Id.is_in(chunk.to_vec()))
            .select_only()
            .column_as(file_size_sum_bigint(), "total")
            .into_tuple()
            .one(db)
            .await?
            .flatten();
        total += sum.unwrap_or(0);
    }
    Ok(total)
}

pub async fn instance_used_bytes<C: ConnectionTrait>(db: &C) -> Result<i64, sea_orm::DbErr> {
    use gradient_entity::cached_path::Entity as ECP;
    let sum: Option<i64> = ECP::find()
        .select_only()
        .column_as(file_size_sum_bigint(), "total")
        .into_tuple()
        .one(db)
        .await?
        .flatten();
    Ok(sum.unwrap_or(0))
}

pub async fn project_writable_caches<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MCache>, sea_orm::DbErr> {
    use gradient_entity::cache::{Column as CCache, Entity as ECache};
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
        return Ok(Vec::new());
    }

    ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .filter(CCache::Active.eq(true))
        .all(db)
        .await
}

fn headroom(
    cache_limit_gb: i32,
    cache_used: i64,
    instance_limit_gb: i32,
    instance_used: i64,
) -> i64 {
    let cache_free = limit_to_bytes(cache_limit_gb)
        .map(|lim| lim - cache_used)
        .unwrap_or(i64::MAX);
    let instance_free = limit_to_bytes(instance_limit_gb)
        .map(|lim| lim - instance_used)
        .unwrap_or(i64::MAX);
    cache_free.min(instance_free)
}

pub async fn project_caches_all_full<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_limit_gb: i32,
) -> Result<bool, sea_orm::DbErr> {
    let caches = project_writable_caches(db, project).await?;
    if caches.is_empty() {
        return Ok(false);
    }
    let instance_used = instance_used_bytes(db).await?;
    for cache in &caches {
        let used = cache_used_bytes(db, cache.id).await?;
        let free = headroom(cache.max_storage_gb, used, instance_limit_gb, instance_used);
        if free >= STORAGE_HEADROOM_BYTES {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn unconfirmed_cached_path_count<C: ConnectionTrait>(
    db: &C,
) -> Result<u64, sea_orm::DbErr> {
    ECachedPath::find()
        .filter(CCachedPath::Confirmed.eq(false))
        .count(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_size_sum_casts_to_bigint() {
        use gradient_entity::cached_path::Entity as ECP;
        use sea_orm::{DatabaseBackend, EntityTrait, QuerySelect, QueryTrait};
        let sql = ECP::find()
            .select_only()
            .column_as(file_size_sum_bigint(), "total")
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.to_uppercase().contains("CAST"), "missing cast: {sql}");
        assert!(
            sql.to_lowercase().contains("bigint"),
            "missing bigint: {sql}"
        );
    }

    #[test]
    fn zero_limit_is_unlimited() {
        assert_eq!(limit_to_bytes(0), None);
        assert_eq!(limit_to_bytes(-5), None);
        assert_eq!(limit_to_bytes(1), Some(BYTES_PER_GB));
    }

    #[test]
    fn headroom_bounded_by_tighter_axis() {
        let five_mb = 5 * 1024 * 1024;
        let used = BYTES_PER_GB - five_mb;
        assert_eq!(headroom(1, used, 0, 0), five_mb);
    }

    #[test]
    fn headroom_instance_axis_can_dominate() {
        let one_mb = 1024 * 1024;
        let inst_used = BYTES_PER_GB - one_mb;
        assert_eq!(headroom(0, 0, 1, inst_used), one_mb);
    }

    #[test]
    fn both_unlimited_is_max() {
        assert_eq!(headroom(0, 9_999, 0, 9_999), i64::MAX);
    }
}
