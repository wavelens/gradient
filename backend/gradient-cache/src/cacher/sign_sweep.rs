/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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

const SIGN_SWEEP_BATCH: u64 = 1000;

gradient_db::sql! {
    /// A NAR committed without a resolvable owning job got no cache claim. The signing pass is only
    /// filling existing rows and cannot repair that. Such a path would 404 from the narinfo
    /// endpoint forever. The anti-join is keeping a healthy instance at one indexed probe.
    REPAIR_ORPHAN_CLAIMS = r#"
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

async fn repair_orphan_claims(state: &Arc<ServerState>) -> anyhow::Result<()> {
    let res = state
        .worker_db
        .execute_raw(
            REPAIR_ORPHAN_CLAIMS.bind([sea_orm::Value::BigInt(Some(SIGN_SWEEP_BATCH as i64))]),
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

pub async fn sign_missing_signatures(state: Arc<ServerState>) -> anyhow::Result<()> {
    if let Err(e) = repair_orphan_claims(&state).await {
        warn!(error = %e, "sign sweep: orphan claim repair failed");
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

    let hashes: Vec<String> = cached_paths.values().map(|cp| cp.hash.clone()).collect();
    let private =
        gradient_db::graph::reachability::private_output_hashes(&state.worker_db, &hashes).await?;
    let skipped: HashSet<CachedPathId> = cached_paths
        .values()
        .filter(|cp| private.contains(&cp.hash))
        .map(|cp| cp.id)
        .collect();

    let mut signers: HashMap<CacheId, Option<CacheSigner>> = HashMap::new();
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

        let refs =
            gradient_db::graph::runtime_closure::references_for_hash(&state.worker_db, &cp.hash)
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

#[cfg(test)]
mod orphan_claim_tests {
    use super::REPAIR_ORPHAN_CLAIMS;

    /// An unbounded fixpoint over `cached_path` starved this scheduler before. The pass is running
    /// on a timer.
    #[test]
    fn the_repair_is_bounded_and_respects_the_sign_cache_opt_out() {
        let sql = REPAIR_ORPHAN_CLAIMS.text();
        assert!(sql.contains("LIMIT $1"), "{sql}");
        assert!(
            sql.contains("t.sign_cache"),
            "a task that opted out of signing must not be handed a claim"
        );
    }
}
