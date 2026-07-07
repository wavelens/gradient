/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Periodic backfill that signs `cached_path_signature` placeholder rows.
//!
//! NAR uploads and new cache subscriptions insert `cached_path_signature`
//! rows with `signature = NULL` - "this (path, cache) pair needs a
//! signature". A freshly uploaded NAR is signed in place by the proto upload
//! handler (`sign_cached_path`); this periodic pass is the backfill that
//! catches subscription placeholders and any row a commit left NULL. It walks
//! the pending rows, computes narinfo signatures with the cache's private key,
//! and fills them in, and records `cache_derivation` rows when a derivation's
//! full closure has become cached for a given cache.

use gradient_core::ServerState;
use gradient_sources::CacheSigner;
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect, Set,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, warn};

/// Max pending rows processed per sweep pass. Bounds memory + time per
/// invocation; remaining rows are picked up by the next scheduled pass.
const SIGN_SWEEP_BATCH: u64 = 1000;

/// Skip a `cached_path` iff every producing project has `sign_cache=false`
/// and at least one such project exists. Paths absent from `producers`
/// (i.e. not produced by any project - `.drv` files, direct builds) are
/// signed normally.
pub(crate) fn compute_skipped_cached_paths(
    producers: &HashMap<CachedPathId, Vec<bool>>,
) -> HashSet<CachedPathId> {
    producers
        .iter()
        .filter(|(_, flags)| !flags.is_empty() && flags.iter().all(|f| !f))
        .map(|(id, _)| *id)
        .collect()
}

/// One pass: sign every pending `cached_path_signature` row and update
/// `cache_derivation` where newly-signed paths complete a derivation
/// closure. Errors on individual rows are logged and skipped.
pub async fn sign_missing_signatures(state: Arc<ServerState>) -> anyhow::Result<()> {
    let pending = ECachedPathSignature::find()
        .filter(CCachedPathSignature::Signature.is_null())
        .limit(SIGN_SWEEP_BATCH)
        .all(&state.worker_db)
        .await?;

    if pending.is_empty() {
        return Ok(());
    }

    let cache_ids: Vec<CacheId> = pending
        .iter()
        .map(|r| r.cache)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let cached_path_ids: Vec<CachedPathId> = pending
        .iter()
        .map(|r| r.cached_path)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let caches: HashMap<CacheId, MCache> = ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .all(&state.worker_db)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();

    let cached_paths: HashMap<CachedPathId, MCachedPath> = ECachedPath::find()
        .filter(CCachedPath::Id.is_in(cached_path_ids))
        .all(&state.worker_db)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();

    let producers = load_producing_project_flags(&state, &cached_paths).await?;
    let skipped: HashSet<CachedPathId> = compute_skipped_cached_paths(&producers);

    // Build a per-cache signer once (one crypt-secret read + one private-key
    // decryption per cache, not per row). `None` marks caches whose key
    // failed to decode - we skip their rows for this pass.
    let mut signers: HashMap<CacheId, Option<CacheSigner>> = HashMap::new();
    for (cache_id, cache) in &caches {
        if cache.private_key.is_empty() {
            signers.insert(*cache_id, None);
            continue;
        }
        let signer = match CacheSigner::from_cache(
            &state.config.secrets.crypt_secret_file,
            cache,
            &state.config.server.serve_url,
        ) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(cache_name = %cache.name, error = %e, "sign sweep: failed to prepare signer");
                None
            }
        };
        signers.insert(*cache_id, signer);
    }

    let mut touched_caches: HashSet<CacheId> = HashSet::new();
    let mut signed_hashes: Vec<String> = Vec::new();
    let mut signed = 0usize;

    for row in pending {
        let Some(cache) = caches.get(&row.cache) else {
            continue;
        };
        let Some(Some(signer)) = signers.get(&row.cache) else {
            continue;
        };

        let Some(cp) = cached_paths.get(&row.cached_path) else {
            continue;
        };
        let store_path = cp.store_path();

        if skipped.contains(&row.cached_path) {
            debug!(
                store_path = %store_path,
                cache = %cache.id,
                "sign sweep: skipping (project sign_cache=false)"
            );
            continue;
        }

        let (Some(nar_hash), Some(nar_size)) = (cp.nar_hash.as_deref(), cp.nar_size) else {
            continue;
        };

        let refs = gradient_db::references_for_hash(&state.worker_db, &cp.hash)
            .await
            .unwrap_or_default();

        let nar_hash_nix32 = normalize_nar_hash(nar_hash);

        let sig_bytes =
            signer.sign_narinfo_raw(&store_path, &nar_hash_nix32, nar_size as u64, &refs);

        let mut am = row.into_active_model();
        am.signature = Set(Some(sig_bytes));
        if let Err(e) = am.update(&state.worker_db).await {
            warn!(store_path = %store_path, cache = %cache.id, error = %e, "sign sweep: failed to persist signature");
            continue;
        }

        debug!(cache_name = %cache.name, store_path = %store_path, "sign sweep: signed");
        touched_caches.insert(cache.id);
        signed_hashes.push(cp.hash.clone());
        signed += 1;
    }

    if signed > 0 {
        tracing::info!(count = signed, "sign sweep: signatures filled");
    }

    // Update cache_derivation where this pass's newly signed paths complete a
    // derivation closure. Seeded by the signed outputs and walked up through
    // dependents; the unseeded full scan of every org derivation runs hourly
    // as the backfill (fresh subscriptions, rows a crashed sweep missed).
    let seed: Vec<uuid::Uuid> = if signed_hashes.is_empty() {
        vec![]
    } else {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Hash.is_in(signed_hashes))
            .all(&state.worker_db)
            .await?
            .into_iter()
            .map(|o| o.derivation.into_inner())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    };
    let full = full_backfill_due();
    for cache_id in touched_caches {
        if let Err(e) = record_newly_completed_derivations(&state, cache_id, &seed, full).await {
            warn!(cache = %cache_id, error = %e, "sign sweep: cache_derivation update failed");
        }
    }

    Ok(())
}

/// The full backfill scans every derivation of every subscribed org (minutes
/// on a large DB), so it runs at most once per hour; the per-sweep frontier
/// walk covers everything the sweep itself changed.
const FULL_BACKFILL_SECS: u64 = 3600;

fn full_backfill_due() -> bool {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    match *last {
        // Arm on first call instead of running: a restart must not pay the
        // full scan immediately (frontier passes cover the steady state).
        None => {
            *last = Some(std::time::Instant::now());
            false
        }
        Some(t) if t.elapsed().as_secs() < FULL_BACKFILL_SECS => false,
        Some(_) => {
            *last = Some(std::time::Instant::now());
            true
        }
    }
}

/// For every derivation built by an organization subscribed to `cache_id`
/// whose outputs are all cached and whose dependency closure is already
/// recorded, insert a `cache_derivation` row (org scoping mirrors
/// `gradient_db::derivation_ids_for_org`: project -> evaluation -> build_job).
/// Idempotent. `full = false` restricts the walk to `seed` and its dependents,
/// layer by layer to a fixpoint; `full = true` is the unseeded hourly backfill.
async fn record_newly_completed_derivations(
    state: &ServerState,
    cache_id: CacheId,
    seed: &[uuid::Uuid],
    full: bool,
) -> anyhow::Result<()> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    const INSERT_LAYER: &str = r#"
        INSERT INTO cache_derivation (id, cache, derivation, cached_at)
        SELECT uuidv7(), $1, d.id, $2
        FROM derivation d
        WHERE d.id = ANY($3)
          AND d.id IN (
            SELECT bj.derivation
            FROM build_job bj
            JOIN evaluation ev ON ev.id = bj.evaluation
            JOIN project p ON p.id = ev.project
            JOIN organization_cache oc ON oc.organization = p.organization
            WHERE oc.cache = $1)
          AND NOT EXISTS (
            SELECT 1 FROM derivation_output o
            WHERE o.derivation = d.id AND o.is_cached = false)
          AND NOT EXISTS (
            SELECT 1 FROM derivation_dependency e
            WHERE e.derivation = d.id
              AND NOT EXISTS (
                SELECT 1 FROM cache_derivation cd
                WHERE cd.cache = $1 AND cd.derivation = e.dependency))
          AND NOT EXISTS (
            SELECT 1 FROM cache_derivation cd2
            WHERE cd2.cache = $1 AND cd2.derivation = d.id)
        RETURNING derivation
    "#;

    if full {
        // Set-based shape: materialise the already-recorded set once and hash
        // anti-join against it, instead of a correlated probe per candidate
        // per dependency (which scanned for minutes on a large graph).
        let inserted = state
            .worker_db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
        WITH have AS MATERIALIZED (
            SELECT derivation FROM cache_derivation WHERE cache = $1
        ),
        org_drvs AS (
            SELECT DISTINCT bj.derivation
            FROM build_job bj
            JOIN evaluation ev ON ev.id = bj.evaluation
            JOIN project p ON p.id = ev.project
            JOIN organization_cache oc ON oc.organization = p.organization
            WHERE oc.cache = $1
        )
        INSERT INTO cache_derivation (id, cache, derivation, cached_at)
        SELECT uuidv7(), $1, c.derivation, $2
        FROM org_drvs c
        LEFT JOIN have ON have.derivation = c.derivation
        WHERE have.derivation IS NULL
          AND NOT EXISTS (
            SELECT 1 FROM derivation_output o
            WHERE o.derivation = c.derivation AND o.is_cached = false)
          AND NOT EXISTS (
            SELECT 1 FROM derivation_dependency e
            LEFT JOIN have h ON h.derivation = e.dependency
            WHERE e.derivation = c.derivation AND h.derivation IS NULL)
                "#,
                [cache_id.into_inner().into(), gradient_types::now().into()],
            ))
            .await?
            .rows_affected();
        if inserted > 0 {
            debug!(cache = %cache_id, inserted, "backfilled closure-complete derivations");
        }
        return Ok(());
    }

    let mut frontier: Vec<uuid::Uuid> = seed.to_vec();
    let mut inserted_total = 0usize;
    while !frontier.is_empty() {
        let rows = state
            .worker_db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                INSERT_LAYER,
                [
                    cache_id.into_inner().into(),
                    gradient_types::now().into(),
                    frontier.into(),
                ],
            ))
            .await?;
        if rows.is_empty() {
            break;
        }
        let inserted: Vec<uuid::Uuid> = rows
            .iter()
            .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
            .collect();
        inserted_total += inserted.len();
        frontier = state
            .worker_db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT e.derivation FROM derivation_dependency e WHERE e.dependency = ANY($1)",
                [inserted.into()],
            ))
            .await?
            .iter()
            .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
            .collect();
    }

    if inserted_total > 0 {
        debug!(cache = %cache_id, inserted = inserted_total, "recorded newly closure-complete derivations");
    }
    Ok(())
}

/// Loads, for every cached_path in `cached_paths`, the `sign_cache` flag of
/// every project that produced a matching `derivation_output`. Cached_paths
/// whose hash matches no `derivation_output` (e.g. `.drv` files) are absent
/// from the returned map - that means "no producing project, sign normally".
async fn load_producing_project_flags(
    state: &Arc<ServerState>,
    cached_paths: &HashMap<CachedPathId, MCachedPath>,
) -> anyhow::Result<HashMap<CachedPathId, Vec<bool>>> {
    use sea_orm::{ConnectionTrait, FromQueryResult, Statement};

    let mut out: HashMap<CachedPathId, Vec<bool>> = HashMap::new();
    if cached_paths.is_empty() {
        return Ok(out);
    }

    let cp_by_hash: HashMap<&str, CachedPathId> = cached_paths
        .values()
        .map(|cp| (cp.hash.as_str(), cp.id))
        .collect();
    let hashes: Vec<String> = cp_by_hash.keys().map(|s| s.to_string()).collect();

    #[derive(FromQueryResult)]
    struct Row {
        hash: String,
        sign_cache: bool,
    }

    let backend = state.worker_db.get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        // The reserved per-org `build-request` project backing `gradient build`
        // is always signable regardless of its `sign_cache` flag - its outputs
        // must be substitutable by the submitting client. Keyed on the reserved
        // name (BUILD_REQUEST_PROJECT_NAME), not `managed`, which also marks
        // nix-state-declared projects that may legitimately set sign_cache=false.
        r#"
            SELECT do_.hash AS hash,
                   (p.sign_cache OR p.name = 'build-request') AS sign_cache
            FROM derivation_output do_
            JOIN derivation d   ON d.id = do_.derivation
            JOIN build_job b    ON b.derivation = d.id
            JOIN evaluation e   ON e.id = b.evaluation
            JOIN project p      ON p.id = e.project
            WHERE do_.hash = ANY($1)
        "#,
        [hashes.into()],
    );

    let rows = Row::find_by_statement(stmt).all(&state.worker_db).await?;

    for r in rows {
        if let Some(&id) = cp_by_hash.get(r.hash.as_str()) {
            out.entry(id).or_default().push(r.sign_cache);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(id: u128) -> CachedPathId {
        CachedPathId::new(uuid::Uuid::from_u128(id))
    }

    #[test]
    fn skip_when_all_producing_projects_private() {
        let mut producers: HashMap<CachedPathId, Vec<bool>> = HashMap::new();
        producers.insert(cp(1), vec![false, false]);
        producers.insert(cp(2), vec![false, true]);
        producers.insert(cp(3), vec![true]);

        let skipped = compute_skipped_cached_paths(&producers);

        assert!(
            skipped.contains(&cp(1)),
            "private-only path must be skipped"
        );
        assert!(!skipped.contains(&cp(2)), "mixed path must be signed");
        assert!(!skipped.contains(&cp(3)), "public-only path must be signed");
        assert!(
            !skipped.contains(&cp(4)),
            "orphan (absent from map) must be signed"
        );
    }

    #[test]
    fn skip_set_empty_when_no_private_producers() {
        let mut producers: HashMap<CachedPathId, Vec<bool>> = HashMap::new();
        producers.insert(cp(1), vec![true]);
        producers.insert(cp(2), vec![true, true]);

        let skipped = compute_skipped_cached_paths(&producers);
        assert!(skipped.is_empty());
    }
}
