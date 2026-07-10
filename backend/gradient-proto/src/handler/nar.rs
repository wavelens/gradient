/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ingest::{IngestInput, SignTargets, ingest_metadata_only};
use chrono::Timelike;
use gradient_core::ServerState;
use gradient_types::ids::{CacheId, OrganizationId};
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement, Value,
};
use tracing::debug;

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

/// Resolves the org's cache and increments the traffic counter. `org_id` is
/// resolved on the session read loop before the commit detaches, so it stays
/// valid even after the job is evicted from the tracker on completion.
pub(super) async fn record_nar_push_metric(
    state: &ServerState,
    org_id: Option<OrganizationId>,
    bytes: i64,
) -> anyhow::Result<()> {
    let Some(org_id) = org_id else {
        return Ok(());
    };

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
    // Atomic accumulate keyed on the (cache, bucket_time) unique index: concurrent
    // NAR commits for the same cache in one minute otherwise race a find-then-insert
    // into a duplicate-key violation (and the update arm loses each other's writes).
    state
        .worker_db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO cache_metric (id, cache, bucket_time, bytes_sent, nar_count) \
             VALUES (uuidv7(), $1, $2, $3, 1) \
             ON CONFLICT (cache, bucket_time) DO UPDATE SET \
                 bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent, \
                 nar_count  = cache_metric.nar_count  + 1",
            [
                Value::Uuid(Some(Box::new(cache_id.into_inner()))),
                bucket.into(),
                bytes.into(),
            ],
        ))
        .await?;

    Ok(())
}

pub(super) async fn mark_nar_stored(
    state: &ServerState,
    org_id: Option<OrganizationId>,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    let hash_name = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let hash = hash_name.split('-').next().unwrap_or("");

    if hash.is_empty() {
        return Ok(());
    }

    let targets = match org_id {
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

    let marked = EDerivationOutput::update_many()
        .col_expr(CDerivationOutput::IsCached, Expr::value(true))
        .col_expr(CDerivationOutput::CachedPath, Expr::value(cached_path_id))
        .filter(CDerivationOutput::Hash.eq(hash))
        .exec(&state.worker_db)
        .await?
        .rows_affected;
    if marked > 0 {
        debug!(
            store_path,
            file_size = record.file_size,
            count = marked,
            "derivation_outputs marked cached after NarPush"
        );
    }

    debug!(
        store_path,
        "cached_path metadata recorded after NarUploaded"
    );

    // Sign this specific path in place so its narinfo is servable immediately,
    // rather than waking a whole-table sweep. Placeholder rows only exist when a
    // cache took it (OrgCaches); the periodic sweep stays the backfill for
    // subscription placeholders and anything left NULL.
    if org_id.is_some() {
        crate::signing::sign_cached_path(
            &state.worker_db,
            &state.config.secrets.crypt_secret_file,
            &state.config.server.serve_url,
            crate::signing::SignRequest {
                cached_path: cached_path_id,
                store_path,
                nar_hash: record.nar_hash,
                nar_size: record.nar_size,
                references: record.references,
            },
        )
        .await;
    }
    Ok(())
}
