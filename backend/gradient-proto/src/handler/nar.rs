/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Timelike;
use crate::ingest::{IngestInput, SignTargets, ingest_metadata_only};
use gradient_sources::CacheSigner;
use gradient_types::ids::{CacheId, CacheMetricId, CachedPathId, OrganizationId};
use gradient_types::*;
use gradient_core::ServerState;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, IntoActiveModel,
    QueryFilter, Set, Statement,
};
use std::collections::{HashMap, HashSet};
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

    debug!(store_path, "cached_path metadata recorded after NarUploaded");

    // Sign this specific path in place so its narinfo is servable immediately,
    // rather than waking a whole-table sweep. Placeholder rows only exist when a
    // cache took it (OrgCaches); the periodic sweep stays the backfill for
    // subscription placeholders and anything left NULL.
    if org_id.is_some() {
        sign_cached_path(state, cached_path_id, hash, store_path, record).await;
    }
    Ok(())
}

/// Fill the pending `cached_path_signature` rows for one freshly cached path.
/// Skips paths whose every producing project has `sign_cache=false` (the
/// reserved `build-request` project is always signable). Signing failures are
/// logged, never propagated: the NAR is already stored and the periodic sweep
/// re-signs whatever is left NULL.
async fn sign_cached_path(
    state: &ServerState,
    cached_path_id: CachedPathId,
    hash: &str,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) {
    if producing_projects_all_private(&state.worker_db, hash).await {
        return;
    }

    let pending = match ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path_id))
        .filter(CCachedPathSignature::Signature.is_null())
        .all(&state.worker_db)
        .await
    {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => return,
        Err(e) => {
            warn!(store_path, error = %e, "eager sign: load pending signatures failed");
            return;
        }
    };

    // The just-stored references (hash-name form) are what the narinfo serve
    // path reconstructs the fingerprint from; `fingerprint` sorts them, so this
    // matches the sweep byte-for-byte without re-reading `cached_path_reference`.
    let nar_hash_nix32 = normalize_nar_hash(record.nar_hash);
    let nar_size = record.nar_size as u64;

    // One signer per distinct cache (one crypt-secret read + key decrypt each),
    // built up front; caches whose key is absent/undecodable are simply absent.
    let mut signers: HashMap<CacheId, CacheSigner> = HashMap::new();
    for cache_id in pending.iter().map(|r| r.cache).collect::<HashSet<_>>() {
        if let Some(signer) = build_signer(state, cache_id).await {
            signers.insert(cache_id, signer);
        }
    }

    for row in pending {
        let Some(signer) = signers.get(&row.cache) else {
            continue;
        };

        let sig = signer.sign_narinfo_raw(store_path, &nar_hash_nix32, nar_size, record.references);
        let mut am = row.into_active_model();
        am.signature = Set(Some(sig));
        if let Err(e) = am.update(&state.worker_db).await {
            warn!(store_path, error = %e, "eager sign: persist signature failed");
        }
    }
}

/// Build a signer for `cache_id`, or `None` when the cache is gone or its key is
/// empty/undecodable (the periodic sweep logs the same and skips those rows).
async fn build_signer(state: &ServerState, cache_id: CacheId) -> Option<CacheSigner> {
    let cache = ECache::find_by_id(cache_id)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()?;
    if cache.private_key.is_empty() {
        return None;
    }
    match CacheSigner::from_cache(
        &state.config.secrets.crypt_secret_file,
        &cache,
        &state.config.server.serve_url,
    ) {
        Ok(s) => Some(s),
        Err(e) => {
            warn!(cache_name = %cache.name, error = %e, "eager sign: failed to prepare signer");
            None
        }
    }
}

/// True iff the path is produced by at least one project and every producing
/// project has `sign_cache=false` - mirrors the sweep's skip gate. The reserved
/// per-org `build-request` project is always signable (its outputs must be
/// substitutable by the submitting client). Paths with no producing project
/// (`.drv` files, direct builds) return false → signed normally.
async fn producing_projects_all_private<C: ConnectionTrait>(db: &C, hash: &str) -> bool {
    #[derive(FromQueryResult)]
    struct Flags {
        producers: i64,
        signable: i64,
    }

    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        r#"
            SELECT count(*)::bigint AS producers,
                   count(*) FILTER (
                       WHERE p.sign_cache OR p.name = 'build-request'
                   )::bigint AS signable
            FROM derivation_output do_
            JOIN derivation d ON d.id = do_.derivation
            JOIN build_job b  ON b.derivation = d.id
            JOIN evaluation e ON e.id = b.evaluation
            JOIN project p    ON p.id = e.project
            WHERE do_.hash = $1
        "#,
        [hash.into()],
    );

    match Flags::find_by_statement(stmt).one(db).await {
        Ok(Some(f)) => f.producers > 0 && f.signable == 0,
        Ok(None) => false,
        Err(e) => {
            warn!(%hash, error = %e, "eager sign: producer-flag query failed; signing");
            false
        }
    }
}
