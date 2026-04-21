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
    /// Store-path references in hash-name format (no `/nix/store/` prefix).
    pub references: &'a [String],
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

/// Update the `derivation_output` and `cached_path` records for `store_path`
/// after a NAR push.  Creates `cached_path` and `cached_path_signature` rows
/// when the worker supplies path metadata (build-output uploads from remote
/// workers).
pub(super) async fn mark_nar_stored(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
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

    let hash_name = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let hash = hash_name.split('-').next().unwrap_or("");
    let package = hash_name
        .find('-')
        .map(|i| &hash_name[i + 1..])
        .unwrap_or("");

    if hash.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().naive_utc();

    // Find or create the cached_path row.
    let references_str = if record.references.is_empty() {
        None
    } else {
        Some(record.references.join(" "))
    };

    let cached_path_row = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.db)
        .await?
    {
        Some(row) => {
            let mut active = row.into_active_model();
            active.file_size = Set(Some(record.file_size));
            active.file_hash = Set(Some(record.file_hash.to_owned()));
            active.nar_size = Set(Some(record.nar_size));
            active.nar_hash = Set(Some(record.nar_hash.to_owned()));
            if references_str.is_some() {
                active.references = Set(references_str);
            }
            active.update(&state.db).await?
        }
        None => {
            let am = ACachedPath {
                id: Set(Uuid::new_v4()),
                store_path: Set(store_path.to_owned()),
                hash: Set(hash.to_owned()),
                package: Set(package.to_owned()),
                file_hash: Set(Some(record.file_hash.to_owned())),
                file_size: Set(Some(record.file_size)),
                nar_size: Set(Some(record.nar_size)),
                nar_hash: Set(Some(record.nar_hash.to_owned())),
                references: Set(references_str),
                ca: Set(None),
                created_at: Set(now),
            };
            match am.insert(&state.db).await {
                Ok(row) => row,
                Err(e) => {
                    warn!(store_path, error = %e, "failed to insert cached_path (may be a race)");
                    // Try to find the row that was inserted concurrently.
                    match ECachedPath::find()
                        .filter(CCachedPath::Hash.eq(hash))
                        .one(&state.db)
                        .await?
                    {
                        Some(row) => row,
                        None => return Err(e.into()),
                    }
                }
            }
        }
    };

    // Enqueue signing: insert a placeholder `cached_path_signature` row
    // (signature = NULL) for every cache owned by the job's org. The
    // periodic sign sweep (see `cache::cacher::sign_sweep`) will fill in
    // the signatures in the background.
    ensure_signature_placeholders(state, scheduler, job_id, &cached_path_row, now).await;

    info!(
        store_path,
        "cached_path metadata recorded after NarUploaded"
    );
    Ok(())
}

/// Insert `cached_path_signature` placeholders (signature = NULL) for every
/// cache the job's organization is subscribed to. The periodic sweep will
/// sign them later. Existing rows are left untouched.
async fn ensure_signature_placeholders(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    cached_path_row: &MCachedPath,
    now: chrono::NaiveDateTime,
) {
    let Some(org_id) = scheduler.peer_id_for_job(job_id).await else {
        return;
    };

    let org_caches = match EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .all(&state.db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(%org_id, error = %e, "failed to fetch org caches for signature placeholders");
            return;
        }
    };

    for oc in org_caches {
        let exists = ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cached_path_row.id))
            .filter(CCachedPathSignature::Cache.eq(oc.cache))
            .one(&state.db)
            .await
            .unwrap_or(None)
            .is_some();
        if exists {
            continue;
        }

        let am = ACachedPathSignature {
            id: Set(Uuid::new_v4()),
            cached_path: Set(cached_path_row.id),
            cache: Set(oc.cache),
            signature: Set(None),
            created_at: Set(now),
        };
        if let Err(e) = am.insert(&state.db).await {
            warn!(
                store_path = %cached_path_row.store_path,
                cache = %oc.cache,
                error = %e,
                "failed to insert cached_path_signature placeholder"
            );
        }
    }
}
