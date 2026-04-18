/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Timelike;
use gradient_core::types::*;
use scheduler::Scheduler;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use tracing::{info, warn};
use uuid::Uuid;

/// Metadata produced by a worker after compressing and uploading a NAR.
pub(super) struct NarUploadRecord<'a> {
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
}

/// Create `cached_path` rows for source paths pushed during evaluation.
///
/// Resolves `job_id → org → caches` and creates one `cached_path` row per
/// cache for each fetched input.
pub(super) async fn record_fetched_paths(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    fetched_paths: &[gradient_core::types::proto::FetchedInput],
) -> anyhow::Result<()> {
    use gradient_core::sources::get_hash_from_path;

    if fetched_paths.is_empty() {
        return Ok(());
    }

    let org_id = scheduler
        .peer_id_for_job(job_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("no peer for job {}", job_id))?;

    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .all(&state.db)
        .await?;

    if org_caches.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().naive_utc();

    for fi in fetched_paths {
        let (hash, package) = match get_hash_from_path(fi.store_path.clone()) {
            Ok(v) => v,
            Err(e) => {
                warn!(store_path = %fi.store_path, error = %e, "cannot parse fetched path");
                continue;
            }
        };

        let Some(row) = find_or_create_cached_path(state, fi, &hash, &package, now).await? else {
            continue;
        };
        record_cached_path_signatures(state, &row, &org_caches, fi, now).await;
    }

    info!(count = fetched_paths.len(), %org_id, "recorded fetched paths in cache");
    Ok(())
}

async fn find_or_create_cached_path(
    state: &ServerState,
    fi: &gradient_core::types::proto::FetchedInput,
    hash: &str,
    package: &str,
    now: chrono::NaiveDateTime,
) -> anyhow::Result<Option<entity::cached_path::Model>> {
    match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.db)
        .await?
    {
        Some(row) => Ok(Some(row)),
        None => {
            let am = ACachedPath {
                id: Set(Uuid::new_v4()),
                store_path: Set(fi.store_path.clone()),
                hash: Set(hash.to_owned()),
                package: Set(package.to_owned()),
                file_hash: Set(None),
                file_size: Set(None),
                nar_size: Set(Some(fi.nar_size as i64)),
                nar_hash: Set(Some(fi.nar_hash.clone())),
                references: Set(None),
                ca: Set(None),
                created_at: Set(now),
            };
            match am.insert(&state.db).await {
                Ok(row) => Ok(Some(row)),
                Err(e) => {
                    warn!(store_path = %fi.store_path, error = %e, "failed to insert cached_path");
                    Ok(None)
                }
            }
        }
    }
}

async fn record_cached_path_signatures(
    state: &ServerState,
    cached_path_row: &entity::cached_path::Model,
    org_caches: &[entity::organization_cache::Model],
    fi: &gradient_core::types::proto::FetchedInput,
    now: chrono::NaiveDateTime,
) {
    for oc in org_caches {
        let existing = ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cached_path_row.id))
            .filter(CCachedPathSignature::Cache.eq(oc.cache))
            .one(&state.db)
            .await
            .unwrap_or(None);

        if existing.is_some() {
            continue;
        }

        let sig_row = ACachedPathSignature {
            id: Set(Uuid::new_v4()),
            cached_path: Set(cached_path_row.id),
            cache: Set(oc.cache),
            signature: Set(fi.signature.clone()),
            created_at: Set(now),
        };
        if let Err(e) = sig_row.insert(&state.db).await {
            warn!(
                store_path = %fi.store_path,
                cache = %oc.cache,
                error = %e,
                "failed to insert cached_path_signature"
            );
        }
    }
}

/// Record a cache metric entry for a NAR push (direct or presigned).
///
/// Resolves `job_id → org → cache` and increments the traffic counter.
pub(super) async fn record_nar_push_metric(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    bytes: i64,
) -> anyhow::Result<()> {
    let org_id = scheduler
        .peer_id_for_job(job_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("no peer for job {}", job_id))?;

    let org_cache = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no cache for org {}", org_id))?;

    let cache_id = org_cache.cache;
    let now = chrono::Utc::now().naive_utc();
    let bucket = now
        .with_second(0)
        .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
        .unwrap_or(now);

    upsert_cache_metric(state, cache_id, bucket, bytes).await
}

async fn upsert_cache_metric(
    state: &ServerState,
    cache_id: Uuid,
    bucket: chrono::NaiveDateTime,
    bytes: i64,
) -> anyhow::Result<()> {
    match ECacheMetric::find()
        .filter(CCacheMetric::Cache.eq(cache_id))
        .filter(CCacheMetric::BucketTime.eq(bucket))
        .one(&state.db)
        .await?
    {
        Some(metric) => {
            let mut am: ACacheMetric = metric.into_active_model();
            am.bytes_sent = Set(am.bytes_sent.unwrap() + bytes);
            am.nar_count = Set(am.nar_count.unwrap() + 1);
            am.update(&state.db).await?;
        }
        None => {
            let am = ACacheMetric {
                id: Set(Uuid::new_v4()),
                cache: Set(cache_id),
                bucket_time: Set(bucket),
                bytes_sent: Set(bytes),
                nar_count: Set(1),
            };
            am.insert(&state.db).await?;
        }
    }

    Ok(())
}

/// Update the `derivation_output` or `cached_path` record for `store_path`
/// after a direct NAR push.
pub(super) async fn mark_nar_stored(
    state: &ServerState,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    if let Some(row) = EDerivationOutput::find()
        .filter(CDerivationOutput::Output.eq(store_path))
        .one(&state.db)
        .await?
    {
        let mut active = row.into_active_model();
        active.is_cached = Set(true);
        active.file_size = Set(Some(record.file_size));
        active.update(&state.db).await?;
        info!(
            store_path,
            file_size = record.file_size,
            "derivation_output marked cached after NarPush"
        );
    }

    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()
        .unwrap_or("");

    if !hash.is_empty() {
        let cached_rows = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash))
            .all(&state.db)
            .await?;

        for row in cached_rows {
            let mut active = row.into_active_model();
            active.file_size = Set(Some(record.file_size));
            active.file_hash = Set(Some(record.file_hash.to_owned()));
            active.nar_size = Set(Some(record.nar_size));
            active.nar_hash = Set(Some(record.nar_hash.to_owned()));
            active.update(&state.db).await?;
        }
    }

    Ok(())
}
