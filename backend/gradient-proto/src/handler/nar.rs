/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Timelike;
use crate::ingest::{IngestInput, SignTargets, ingest_metadata_only};
use gradient_types::ids::{CacheId, CacheMetricId};
use gradient_types::*;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use tracing::{debug, warn};

pub(super) struct NarUploadRecord<'a> {
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
    /// Store-path references in hash-name format (no `/nix/store/` prefix).
    pub references: &'a [String],
    /// Full deriver `.drv` path, if the worker reported one.
    pub deriver: Option<&'a str>,
}

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
        .one(&state.worker_db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no cache for org {}", org_id))?;

    let cache_id = org_cache.cache;
    let now = gradient_types::now();
    let bucket = now
        .with_second(0)
        .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
        .unwrap_or(now);

    upsert_cache_metric(state, cache_id, bucket, bytes).await
}

async fn upsert_cache_metric(
    state: &ServerState,
    cache_id: CacheId,
    bucket: chrono::NaiveDateTime,
    bytes: i64,
) -> anyhow::Result<()> {
    match ECacheMetric::find()
        .filter(CCacheMetric::Cache.eq(cache_id))
        .filter(CCacheMetric::BucketTime.eq(bucket))
        .one(&state.worker_db)
        .await?
    {
        Some(metric) => {
            let mut am: ACacheMetric = metric.into_active_model();
            am.bytes_sent = Set(am.bytes_sent.unwrap() + bytes);
            am.nar_count = Set(am.nar_count.unwrap() + 1);
            am.update(&state.worker_db).await?;
        }
        None => {
            let am = MCacheMetric {
                id: CacheMetricId::now_v7(),
                cache: cache_id,
                bucket_time: bucket,
                bytes_sent: bytes,
                nar_count: 1,
            }
            .into_active_model();

            am.insert(&state.worker_db).await?;
        }
    }

    Ok(())
}

pub(super) async fn mark_nar_stored(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    let hash_name = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let hash = hash_name.split('-').next().unwrap_or("");

    if hash.is_empty() {
        return Ok(());
    }

    let targets = match scheduler.peer_id_for_job(job_id).await {
        Some(org_id) => SignTargets::OrgCaches(org_id),
        None => SignTargets::None,
    };

    let input = IngestInput {
        store_path,
        file_hash: record.file_hash,
        file_size: record.file_size,
        nar_size: record.nar_size,
        nar_hash: record.nar_hash,
        references: record.references,
        deriver: record.deriver,
    };

    let cached_path_id = ingest_metadata_only(&state.worker_db, input, targets)
        .await?
        .cached_path;

    let outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Hash.eq(hash))
        .all(&state.worker_db)
        .await?;
    let mut marked = 0usize;
    for row in outputs {
        let mut active = row.into_active_model();
        active.is_cached = Set(true);
        active.cached_path = Set(Some(cached_path_id));
        if let Err(e) = active.update(&state.worker_db).await {
            warn!(store_path, error = %e, "failed to mark derivation_output cached");
        } else {
            marked += 1;
        }
    }
    if marked > 0 {
        debug!(
            store_path,
            file_size = record.file_size,
            count = marked,
            "derivation_outputs marked cached after NarPush"
        );
    }

    debug!(store_path, "cached_path metadata recorded after NarUploaded");
    Ok(())
}
