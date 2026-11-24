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
use std::time::{Duration, Instant};

pub const STORAGE_HEADROOM_BYTES: i64 = 10 * 1024 * 1024;
const FREE_BYTES_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(60);

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

pub struct FreeStorageBytesSnapshot {
    taken_at: Instant,
    instance_free: i64,
    caches: Vec<(String, i64)>,
}

impl FreeStorageBytesSnapshot {
    fn has_room_for(&self, declared: i64) -> bool {
        let required = declared.saturating_add(STORAGE_HEADROOM_BYTES);
        self.caches.is_empty()
            || (self.instance_free >= required
                && self.caches.iter().any(|(_, free)| *free >= required))
    }

    pub fn try_reserve_upload_bytes_in_instance_and_cache_with_headroom(
        &mut self,
        declared: u64,
    ) -> bool {
        let declared = i64::try_from(declared).unwrap_or(i64::MAX);
        if !self.has_room_for(declared) {
            return false;
        }
        self.instance_free = self.instance_free.saturating_sub(declared);
        for (_, free) in &mut self.caches {
            *free = free.saturating_sub(declared);
        }
        true
    }

    fn cache_names(&self) -> Vec<String> {
        self.caches.iter().map(|(name, _)| name.clone()).collect()
    }
}

fn free_bytes_under_limit(limit_gb: i64, used: i64) -> i64 {
    limit_to_bytes(limit_gb).map_or(i64::MAX, |limit| limit - used)
}

async fn instance_free_bytes<C: ConnectionTrait>(
    db: &C,
    instance_limit_gb: u32,
) -> Result<i64, sea_orm::DbErr> {
    if instance_limit_gb == 0 {
        return Ok(i64::MAX);
    }
    let used = instance_used_bytes(db).await?;
    Ok(free_bytes_under_limit(i64::from(instance_limit_gb), used))
}

async fn cache_free_bytes<C: ConnectionTrait>(
    db: &C,
    cache: &MCache,
) -> Result<i64, sea_orm::DbErr> {
    if cache.max_storage_gb <= 0 {
        return Ok(i64::MAX);
    }
    let used = sum_file_sizes_for_cache(db, cache.id).await?;
    Ok(free_bytes_under_limit(
        i64::from(cache.max_storage_gb),
        used,
    ))
}

async fn read_free_bytes_snapshot_of_caches<C: ConnectionTrait>(
    db: &C,
    caches: Vec<MCache>,
    instance_limit_gb: u32,
) -> Result<FreeStorageBytesSnapshot, sea_orm::DbErr> {
    let taken_at = Instant::now();
    if caches.is_empty() {
        return Ok(FreeStorageBytesSnapshot {
            taken_at,
            instance_free: i64::MAX,
            caches: Vec::new(),
        });
    }
    let instance_free = instance_free_bytes(db, instance_limit_gb).await?;
    let mut free = Vec::with_capacity(caches.len());
    for cache in caches {
        let bytes = cache_free_bytes(db, &cache).await?;
        free.push((cache.name, bytes));
    }
    Ok(FreeStorageBytesSnapshot {
        taken_at,
        instance_free,
        caches: free,
    })
}

pub async fn read_project_writable_caches_free_bytes<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_limit_gb: u32,
) -> Result<FreeStorageBytesSnapshot, sea_orm::DbErr> {
    let caches = project_writable_caches(db, project).await?;
    read_free_bytes_snapshot_of_caches(db, caches, instance_limit_gb).await
}

pub async fn read_cache_free_bytes_snapshot_and_instance_from_database<C: ConnectionTrait>(
    db: &C,
    cache: &MCache,
    instance_limit_gb: u32,
) -> Result<FreeStorageBytesSnapshot, sea_orm::DbErr> {
    read_free_bytes_snapshot_of_caches(db, vec![cache.clone()], instance_limit_gb).await
}

#[derive(Default)]
pub struct FreeBytesForUploads {
    snapshot: Option<FreeStorageBytesSnapshot>,
}

impl FreeBytesForUploads {
    pub async fn admit_upload_to_project_writable_caches<C: ConnectionTrait>(
        &mut self,
        db: &C,
        project: ProjectId,
        instance_limit_gb: u32,
        declared: u64,
    ) -> Result<Option<String>, sea_orm::DbErr> {
        let fresh = self
            .snapshot
            .take()
            .filter(|snapshot| snapshot.taken_at.elapsed() < FREE_BYTES_SNAPSHOT_MAX_AGE);
        let snapshot = match fresh {
            Some(snapshot) => snapshot,
            None => read_project_writable_caches_free_bytes(db, project, instance_limit_gb).await?,
        };
        let snapshot = self.snapshot.insert(snapshot);
        if snapshot.try_reserve_upload_bytes_in_instance_and_cache_with_headroom(declared) {
            return Ok(None);
        }
        Ok(Some(build_upload_refusal_message_for_full_caches(
            &snapshot.cache_names(),
            declared,
        )))
    }
}

pub async fn project_caches_all_full<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_limit_gb: u32,
) -> Result<bool, sea_orm::DbErr> {
    let snapshot = read_project_writable_caches_free_bytes(db, project, instance_limit_gb).await?;
    Ok(!snapshot.has_room_for(0))
}

pub fn build_upload_refusal_message_for_full_caches(caches: &[String], declared: u64) -> String {
    let caches = caches.join(", ");
    format!(
        "Writable caches {caches} lack free space for a {declared}-byte upload plus 10 MiB of safety margin under each storage limit or the instance storage limit. Delete stored paths or raise those limits before the upload."
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

    const MIB: u64 = 1024 * 1024;

    fn make_free_bytes_snapshot_at_with_instance_and_cache_free_bytes(
        taken_at: Instant,
        instance_free: u64,
        cache_free: u64,
    ) -> FreeStorageBytesSnapshot {
        FreeStorageBytesSnapshot {
            taken_at,
            instance_free: instance_free as i64,
            caches: vec![("c".to_owned(), cache_free as i64)],
        }
    }

    #[test]
    fn upload_is_refused_when_declared_size_plus_10_mib_headroom_exceeds_cache_or_instance_free_space()
     {
        let now = Instant::now();
        let mut cache_bound = make_free_bytes_snapshot_at_with_instance_and_cache_free_bytes(
            now,
            u64::MAX >> 1,
            100 * MIB,
        );
        assert!(
            !cache_bound.try_reserve_upload_bytes_in_instance_and_cache_with_headroom(95 * MIB)
        );
        assert!(cache_bound.try_reserve_upload_bytes_in_instance_and_cache_with_headroom(90 * MIB));

        let mut instance_bound = make_free_bytes_snapshot_at_with_instance_and_cache_free_bytes(
            now,
            50 * MIB,
            u64::MAX >> 1,
        );
        assert!(
            !instance_bound.try_reserve_upload_bytes_in_instance_and_cache_with_headroom(45 * MIB)
        );
        assert!(
            instance_bound.try_reserve_upload_bytes_in_instance_and_cache_with_headroom(40 * MIB)
        );
    }

    #[tokio::test]
    async fn second_upload_in_same_ssh_session_is_refused_when_first_upload_uses_all_free_space() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let mut session = FreeBytesForUploads {
            snapshot: Some(
                make_free_bytes_snapshot_at_with_instance_and_cache_free_bytes(
                    Instant::now(),
                    u64::MAX >> 1,
                    100 * MIB,
                ),
            ),
        };
        let project = ProjectId::now_v7();

        let first = session
            .admit_upload_to_project_writable_caches(&connection, project, 0, 60 * MIB)
            .await
            .unwrap();
        let second = session
            .admit_upload_to_project_writable_caches(&connection, project, 0, 60 * MIB)
            .await
            .unwrap();

        assert_eq!(first, None);
        assert!(second.is_some_and(|reason| reason.contains('c')));
        db.finish().await;
    }

    #[tokio::test]
    async fn test_upload_is_admitted_after_rereading_free_space_older_than_60_seconds() {
        use crate::rows::insert;
        use gradient_entity::ids::ProjectCacheId;
        use gradient_entity::{cache, project, project_cache, user};
        use gradient_fixtures::rows;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        insert::<user::Entity, _>(&connection, rows::user())
            .await
            .unwrap();
        let cache = insert::<cache::Entity, _>(
            &connection,
            cache::Model {
                max_storage_gb: 1,
                ..rows::cache_with_id(CacheId::now_v7(), "c", rows::user().id)
            },
        )
        .await
        .unwrap();
        let project = insert::<project::Entity, _>(&connection, rows::project())
            .await
            .unwrap()
            .id;
        insert::<project_cache::Entity, _>(
            &connection,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project,
                cache.id,
                CacheSubscriptionMode::ReadWrite,
            ),
        )
        .await
        .unwrap();
        let expired = Instant::now()
            .checked_sub(FREE_BYTES_SNAPSHOT_MAX_AGE + Duration::from_secs(1))
            .unwrap();
        let mut job = FreeBytesForUploads {
            snapshot: Some(
                make_free_bytes_snapshot_at_with_instance_and_cache_free_bytes(
                    expired,
                    u64::MAX >> 1,
                    0,
                ),
            ),
        };

        let refusal = job
            .admit_upload_to_project_writable_caches(&connection, project, 0, 60 * MIB)
            .await
            .unwrap();

        assert_eq!(refusal, None);
        db.finish().await;
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
        assert!(
            project_caches_all_full(&connection, project.id, 0)
                .await
                .unwrap()
        );

        let mut unlimited = full.into_active_model();
        unlimited.max_storage_gb = Set(0);
        update::<cache::Entity, _>(&connection, unlimited)
            .await
            .unwrap();
        assert!(
            !project_caches_all_full(&connection, project.id, 0)
                .await
                .unwrap()
        );
        db.finish().await;
    }
}
