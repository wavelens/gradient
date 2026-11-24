/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::cache::Model as MCache;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::ids::{CacheId, ProjectId};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect};

pub const STORAGE_HEADROOM_BYTES: i64 = 10 * 1024 * 1024;

/// Postgres is widening `SUM(int8)` to `NUMERIC`.
/// A `NUMERIC` sum is failing to decode into `Option<i64>`.
fn file_size_sum_bigint() -> Expr {
    use gradient_entity::cached_path::Column as CCP;
    use sea_orm::sea_query::ExprTrait;
    CCP::FileSize.sum().cast_as(Alias::new("bigint"))
}

const BYTES_PER_GB: i64 = 1024 * 1024 * 1024;

fn limit_to_bytes(max_storage_gb: i64) -> Option<i64> {
    if max_storage_gb <= 0 {
        None
    } else {
        Some(max_storage_gb * BYTES_PER_GB)
    }
}

pub async fn sum_file_sizes_for_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<i64, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::cached_path_signature::{Column as CSig, Entity as ESig};
    use sea_orm::sea_query::{ExprTrait, Query};

    let signed_in_cache = Query::select()
        .expr(Expr::val(1))
        .from(ESig)
        .and_where(Expr::col((ESig, CSig::CachedPath)).equals((ECP, CCP::Id)))
        .and_where(Expr::col((ESig, CSig::Cache)).eq(cache))
        .to_owned();
    let sum: Option<i64> = ECP::find()
        .select_only()
        .column_as(file_size_sum_bigint(), "total")
        .filter(Expr::exists(signed_in_cache))
        .into_tuple()
        .one(db)
        .await?
        .flatten();
    Ok(sum.unwrap_or(0))
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
    instance_limit_gb: u32,
    instance_used: i64,
) -> i64 {
    let cache_free = limit_to_bytes(i64::from(cache_limit_gb))
        .map(|lim| lim - cache_used)
        .unwrap_or(i64::MAX);
    let instance_free = limit_to_bytes(i64::from(instance_limit_gb))
        .map(|lim| lim - instance_used)
        .unwrap_or(i64::MAX);
    cache_free.min(instance_free)
}

async fn instance_used_bytes_or_zero_when_unlimited<C: ConnectionTrait>(
    db: &C,
    instance_limit_gb: u32,
) -> Result<i64, sea_orm::DbErr> {
    if instance_limit_gb == 0 {
        Ok(0)
    } else {
        instance_used_bytes(db).await
    }
}

async fn cache_has_at_least_headroom_bytes_free<C: ConnectionTrait>(
    db: &C,
    cache: &MCache,
    instance_limit_gb: u32,
    instance_used: i64,
) -> Result<bool, sea_orm::DbErr> {
    let cache_used = if cache.max_storage_gb > 0 {
        sum_file_sizes_for_cache(db, cache.id).await?
    } else {
        0
    };
    let free = headroom(
        cache.max_storage_gb,
        cache_used,
        instance_limit_gb,
        instance_used,
    );
    Ok(free >= STORAGE_HEADROOM_BYTES)
}

pub async fn storage_limits_leave_less_than_required_free_space_for_upload<C: ConnectionTrait>(
    db: &C,
    cache: &MCache,
    instance_limit_gb: u32,
) -> Result<bool, sea_orm::DbErr> {
    let instance_used = instance_used_bytes_or_zero_when_unlimited(db, instance_limit_gb).await?;
    Ok(
        !cache_has_at_least_headroom_bytes_free(db, cache, instance_limit_gb, instance_used)
            .await?,
    )
}

pub async fn project_writable_cache_names_if_all_over_storage_limit<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_limit_gb: u32,
) -> Result<Option<Vec<String>>, sea_orm::DbErr> {
    let caches = project_writable_caches(db, project).await?;
    if caches.is_empty() {
        return Ok(None);
    }
    let instance_used = instance_used_bytes_or_zero_when_unlimited(db, instance_limit_gb).await?;
    for cache in &caches {
        if cache_has_at_least_headroom_bytes_free(db, cache, instance_limit_gb, instance_used)
            .await?
        {
            return Ok(None);
        }
    }
    Ok(Some(caches.into_iter().map(|cache| cache.name).collect()))
}

pub async fn project_caches_all_full<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_limit_gb: u32,
) -> Result<bool, sea_orm::DbErr> {
    Ok(
        project_writable_cache_names_if_all_over_storage_limit(db, project, instance_limit_gb)
            .await?
            .is_some(),
    )
}

pub fn build_upload_refusal_message_from_full_writable_cache_names(caches: &[String]) -> String {
    let caches = caches.join(", ");
    format!(
        "The writable caches {caches} each have less than 10 MiB left under the per-cache storage limit or the instance storage limit. Delete stored paths from them or raise the limits before the upload."
    )
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

    #[tokio::test]
    async fn cache_full_check_sums_signed_path_once_and_unlimited_limit_returns_none() {
        use crate::rows::{insert, update};
        use gradient_entity::ids::{CachedPathId, CachedPathSignatureId, ProjectCacheId};
        use gradient_entity::{
            cache, cached_path, cached_path_signature, project, project_cache, user,
        };
        use gradient_fixtures::rows;
        use sea_orm::{ActiveValue::Set, IntoActiveModel};

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        insert::<user::Entity, _>(&connection, rows::user())
            .await
            .unwrap();
        let full = insert::<cache::Entity, _>(
            &connection,
            cache::Model {
                max_storage_gb: 1,
                ..rows::cache_with_id(CacheId::now_v7(), "c", rows::user().id)
            },
        )
        .await
        .unwrap();
        let other = insert::<cache::Entity, _>(
            &connection,
            rows::cache_with_id(CacheId::now_v7(), "d", rows::user().id),
        )
        .await
        .unwrap();
        let project = insert::<project::Entity, _>(&connection, rows::project())
            .await
            .unwrap();
        insert::<project_cache::Entity, _>(
            &connection,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project.id,
                full.id,
                CacheSubscriptionMode::ReadWrite,
            ),
        )
        .await
        .unwrap();
        let shared_size = BYTES_PER_GB - 5 * 1024 * 1024;
        for (hash, size, caches) in [
            ("0".repeat(32), shared_size, vec![full.id, other.id]),
            ("1".repeat(32), 3 * BYTES_PER_GB, vec![other.id]),
        ] {
            let path = insert::<cached_path::Entity, _>(
                &connection,
                cached_path::Model {
                    id: CachedPathId::now_v7(),
                    nar_object_storage_key: hash.clone(),
                    hash,
                    package: "p".into(),
                    file_size: Some(size),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            for cache in caches {
                insert::<cached_path_signature::Entity, _>(
                    &connection,
                    cached_path_signature::Model {
                        id: CachedPathSignatureId::now_v7(),
                        cached_path: path.id,
                        cache,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            }
        }

        assert_eq!(
            sum_file_sizes_for_cache(&connection, full.id)
                .await
                .unwrap(),
            shared_size
        );
        assert_eq!(
            project_writable_cache_names_if_all_over_storage_limit(&connection, project.id, 0)
                .await
                .unwrap(),
            Some(vec!["c".to_owned()])
        );

        let mut unlimited = full.into_active_model();
        unlimited.max_storage_gb = Set(0);
        update::<cache::Entity, _>(&connection, unlimited)
            .await
            .unwrap();
        assert_eq!(
            project_writable_cache_names_if_all_over_storage_limit(&connection, project.id, 0)
                .await
                .unwrap(),
            None
        );
        db.finish().await;
    }
}
