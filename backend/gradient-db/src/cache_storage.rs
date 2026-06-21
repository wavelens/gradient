/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Storage-accounting helpers for the per-cache / per-instance max-storage
//! gate. Usage is the logical sum of `cached_path.file_size` (compressed NAR
//! bytes): per-cache via the `cached_path_signature` join, instance-wide as a
//! global sum. Backend-agnostic (works for local FS and S3).

use gradient_types::ids::{CacheId, DerivationId, OrganizationId};
use gradient_entity::cache::Model as MCache;
use gradient_entity::organization_cache::CacheSubscriptionMode;
use sea_orm::sea_query::{Alias, SimpleExpr};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect};

/// Park threshold: a cache with less than this much free headroom is "full".
pub const STORAGE_HEADROOM_BYTES: i64 = 10 * 1024 * 1024;

/// `SUM(file_size)` cast back to `BIGINT`: Postgres widens `SUM(int8)` to
/// `NUMERIC`, which would otherwise fail to decode into `Option<i64>`.
fn file_size_sum_bigint() -> SimpleExpr {
    use gradient_entity::cached_path::Column as CCP;
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

/// Sum of compressed NAR bytes attributed to a single cache.
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

/// Sum of compressed NAR bytes stored across the whole instance.
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

/// The active, writable (ReadWrite/WriteOnly) caches an org can push to.
pub async fn org_writable_caches<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<Vec<MCache>, sea_orm::DbErr> {
    use gradient_entity::cache::{Column as CCache, Entity as ECache};
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
        return Ok(Vec::new());
    }

    ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .filter(CCache::Active.eq(true))
        .all(db)
        .await
}

/// Free headroom (bytes) for one cache, bounded by both its own limit and the
/// instance-wide limit. A non-positive limit means unlimited on that axis.
/// Returns `i64::MAX` when both axes are unlimited.
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

/// `true` when the org has at least one writable cache AND every writable cache
/// has less than `STORAGE_HEADROOM_BYTES` free. An empty writable-cache set
/// returns `false` (that case is owned by the NoCache gate).
pub async fn org_caches_all_full<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
    instance_limit_gb: i32,
) -> Result<bool, sea_orm::DbErr> {
    let caches = org_writable_caches(db, organization).await?;
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

/// Why an input the worker reported missing was nonetheless treated as
/// available. Captured by the scheduler's missing-input self-heal so the cause
/// (stale `cached_path` whose object was GC'd or never uploaded, vs a producer
/// trusted `Substituted`/`Completed` while its NAR was never cached) is visible
/// at `warn` without raising the global log level.
#[derive(Debug, Default)]
pub struct MissingInputDiagnosis {
    /// A `cached_path` row exists for this output hash.
    pub cached_path_present: bool,
    /// That row claims a fully-uploaded NAR (`file_hash IS NOT NULL`).
    pub fully_cached: bool,
    /// `derivation_output` rows for this hash, and how many are `is_cached`.
    pub outputs_total: usize,
    pub outputs_cached: usize,
    /// Statuses of builds in this evaluation that produce the output.
    pub producer_build_statuses: Vec<gradient_entity::build::BuildStatus>,
}

/// Snapshot the cache/build state of a missing input `hash` within an
/// evaluation, for diagnostic logging by the missing-input self-heal.
pub async fn diagnose_missing_input<C: ConnectionTrait>(
    db: &C,
    _evaluation_id: gradient_types::ids::EvaluationId,
    hash: &str,
) -> Result<MissingInputDiagnosis, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::derivation_build::{Column as CDB, Entity as EDB};
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};

    let cached_path = ECP::find().filter(CCP::Hash.eq(hash)).one(db).await?;
    let outputs = EDO::find().filter(CDO::Hash.eq(hash)).all(db).await?;
    let outputs_cached = outputs.iter().filter(|o| o.is_cached).count();
    let producer_drvs: Vec<DerivationId> = outputs.iter().map(|o| o.derivation).collect();

    // Anchors are global; the producer's build status is the same regardless of
    // the querying evaluation.
    let producer_build_statuses = if producer_drvs.is_empty() {
        Vec::new()
    } else {
        EDB::find()
            .filter(CDB::Derivation.is_in(producer_drvs))
            .all(db)
            .await?
            .into_iter()
            .map(|b| b.status)
            .collect()
    };

    Ok(MissingInputDiagnosis {
        cached_path_present: cached_path.is_some(),
        fully_cached: cached_path.map(|c| c.is_fully_cached()).unwrap_or(false),
        outputs_total: outputs.len(),
        outputs_cached,
        producer_build_statuses,
    })
}

/// Purge a cached output proven unfetchable, so the next evaluation rebuilds it
/// from scratch as if it had never been cached. Clears `is_cached` /
/// `cached_path` on every `derivation_output` with this store-path `hash`, then
/// deletes the `cached_path` row itself (its `cached_path_signature` rows
/// cascade; the `derivation_output` FK is `ON DELETE SET NULL`). The derivation
/// graph is left intact - only the cache artifact is removed. Returns the
/// producing derivations for logging.
pub async fn demote_cached_output<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::derivation_output::{
        ActiveModel as ADerivationOutput, Column as CDO, Entity as EDO,
    };
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, IntoActiveModel};

    let outputs = EDO::find().filter(CDO::Hash.eq(hash)).all(db).await?;
    let mut producers = Vec::with_capacity(outputs.len());
    for o in outputs {
        producers.push(o.derivation);
        let mut active: ADerivationOutput = o.into_active_model();
        active.is_cached = Set(false);
        active.cached_path = Set(None);
        active.update(db).await?;
    }

    ECP::delete_many().filter(CCP::Hash.eq(hash)).exec(db).await?;

    Ok(producers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Demoting a proven-unfetchable output must remove the NAR object from
    /// storage as well as the `cached_path` row, so a re-eval re-pushes it
    /// instead of trusting a row whose object is gone.
    #[tokio::test]
    async fn demote_deletes_the_nar_object() {
        use gradient_storage::NarStore;
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nars").join(&hash[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{}.nar.zst", &hash[2..]));
        std::fs::write(&file, b"x").unwrap();
        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();

        // A `.drv` hash has no `derivation_output` rows; demote still deletes the
        // `cached_path` row (one exec) and the object.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let producers = demote_cached_output(&db, &nar_storage, hash).await.unwrap();

        assert!(producers.is_empty(), "a .drv has no producing derivation");
        assert!(!file.exists(), "demote must delete the NAR object from storage");
    }

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
        assert!(sql.to_lowercase().contains("bigint"), "missing bigint: {sql}");
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
