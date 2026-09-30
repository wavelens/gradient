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
//! and fills them in.

use gradient_core::ServerState;
use gradient_sources::CacheSigner;
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, Set,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, warn};

/// Max pending rows processed per sweep pass. Bounds memory + time per
/// invocation; remaining rows are picked up by the next scheduled pass.
const SIGN_SWEEP_BATCH: u64 = 1000;

gradient_db::sql! {
    /// Re-create `cached_path_signature` rows for paths that hold none at all.
    ///
    /// A NAR whose owning job could not be resolved at commit time was written to
    /// `cached_path` with no cache claim, and the signing pass below cannot repair
    /// that: it only fills rows that already exist. Such a path is cached according
    /// to every gate flag yet 404s from the narinfo endpoint forever. Bounded the
    /// same way as the signing pass, and driven off the "no rows at all" anti-join
    /// so a healthy instance pays one indexed probe.
    RECONCILE_ORPHAN_CLAIMS = r#"
WITH orphan AS (
    SELECT cp.id
    FROM cached_path cp
    WHERE NOT EXISTS (
        SELECT 1 FROM cached_path_signature s WHERE s.cached_path = cp.id)
    LIMIT $1
), claim AS (
    SELECT DISTINCT o.id AS cached_path, c.id AS cache
    FROM orphan o
    JOIN derivation_output dout ON dout.cached_path = o.id
    JOIN build_job bj           ON bj.derivation = dout.derivation
    JOIN evaluation e           ON e.id = bj.evaluation
    JOIN task t                 ON t.id = e.task
    JOIN project_cache pc       ON pc.project = t.project
    JOIN cache c                ON c.id = pc.cache
    WHERE t.sign_cache
)
INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at)
SELECT uuidv7(), claim.cached_path, claim.cache, 0, (now() AT TIME ZONE 'UTC')
FROM claim
ON CONFLICT (cached_path, cache) DO NOTHING
"#,
        params = [Int(1000)],
        tier = Sweep;
}

async fn reconcile_orphan_claims(state: &Arc<ServerState>) -> anyhow::Result<()> {
    let res = state
        .worker_db
        .execute_raw(
            RECONCILE_ORPHAN_CLAIMS.bind([sea_orm::Value::BigInt(Some(SIGN_SWEEP_BATCH as i64))]),
        )
        .await?;
    if res.rows_affected() > 0 {
        tracing::info!(
            count = res.rows_affected(),
            "sign sweep: re-created cache claims for paths that had none"
        );
    }
    Ok(())
}

/// Skip a `cached_path` iff every producing task has `sign_cache=false`
/// and at least one such task exists. Paths absent from `producers`
/// (i.e. not produced by any task - `.drv` files, direct builds) are
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

/// One pass: sign every pending `cached_path_signature` row. Errors on
/// individual rows are logged and skipped.
pub async fn sign_missing_signatures(state: Arc<ServerState>) -> anyhow::Result<()> {
    // Before signing, give back a claim to any path that lost one, so this same
    // pass signs it rather than leaving it unservable for another interval.
    if let Err(e) = reconcile_orphan_claims(&state).await {
        warn!(error = %e, "sign sweep: orphan claim reconcile failed");
    }

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

    let producers = load_producing_task_flags(&state, &cached_paths).await?;
    let skipped: HashSet<CachedPathId> = compute_skipped_cached_paths(&producers);

    // `None` marks caches whose key failed to decode - we skip their rows for
    // this pass.
    let mut signers: HashMap<CacheId, Option<Arc<CacheSigner>>> = HashMap::new();
    for (cache_id, cache) in &caches {
        if cache.private_key.is_empty() {
            signers.insert(*cache_id, None);
            continue;
        }
        let signer = match CacheSigner::from_cache(
            &state.config.secrets.crypt_file,
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
                "sign sweep: skipping (task sign_cache=false)"
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
        signed += 1;
    }

    if signed > 0 {
        tracing::info!(count = signed, "sign sweep: signatures filled");
    }

    Ok(())
}

gradient_db::sql! {
    /// The reserved per-project `build-request` task backing `gradient build`
    /// is always signable regardless of its `sign_cache` flag - its outputs
    /// must be substitutable by the submitting client. Keyed on the reserved
    /// name (BUILD_REQUEST_TASK_NAME), not `managed`, which also marks
    /// nix-state-declared tasks that may legitimately set sign_cache=false.
    PRODUCING_TASK_FLAGS = r#"
            SELECT do_.hash AS hash,
                   (p.sign_cache OR p.name = 'build-request') AS sign_cache
            FROM derivation_output do_
            JOIN derivation d   ON d.id = do_.derivation
            JOIN build_job b    ON b.derivation = d.id
            JOIN evaluation e   ON e.id = b.evaluation
            JOIN task p      ON p.id = e.task
            WHERE do_.hash = ANY($1)
        "#,
        params = [CachedPathHashes(64)];
}

/// Loads, for every cached_path in `cached_paths`, the `sign_cache` flag of
/// every task that produced a matching `derivation_output`. Cached_paths
/// whose hash matches no `derivation_output` (e.g. `.drv` files) are absent
/// from the returned map - that means "no producing task, sign normally".
async fn load_producing_task_flags(
    state: &Arc<ServerState>,
    cached_paths: &HashMap<CachedPathId, MCachedPath>,
) -> anyhow::Result<HashMap<CachedPathId, Vec<bool>>> {
    use sea_orm::FromQueryResult;

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

    let rows = Row::find_by_statement(PRODUCING_TASK_FLAGS.bind([hashes.into()]))
        .all(&state.worker_db)
        .await?;

    for r in rows {
        if let Some(&id) = cp_by_hash.get(r.hash.as_str()) {
            out.entry(id).or_default().push(r.sign_cache);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod orphan_claim_tests {
    use super::RECONCILE_ORPHAN_CLAIMS;

    /// An unbounded fixpoint over `cached_path` has starved this scheduler
    /// before, and the pass runs on a timer.
    #[test]
    fn the_reconcile_is_bounded_and_respects_the_sign_cache_opt_out() {
        let sql = RECONCILE_ORPHAN_CLAIMS.text();
        assert!(sql.contains("LIMIT $1"), "{sql}");
        assert!(
            sql.contains("t.sign_cache"),
            "a task that opted out of signing must not be handed a claim"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(id: u128) -> CachedPathId {
        CachedPathId::new(uuid::Uuid::from_u128(id))
    }

    #[test]
    fn skip_when_all_producing_tasks_private() {
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
}
