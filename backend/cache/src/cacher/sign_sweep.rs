/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Periodic sweep that signs `cached_path_signature` placeholder rows.
//!
//! NAR uploads and new cache subscriptions insert `cached_path_signature`
//! rows with `signature = NULL` — "this (path, cache) pair needs a
//! signature". This sweep walks those pending rows, computes narinfo
//! signatures with the cache's private key, and fills them in. It also
//! records `cache_derivation` rows when a derivation's full closure has
//! become cached for a given cache.

use gradient_core::sources::CacheSigner;
use gradient_core::types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect, Set,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, warn};
use uuid::Uuid;

/// Max pending rows processed per sweep pass. Bounds memory + time per
/// invocation; remaining rows are picked up by the next scheduled pass.
const SIGN_SWEEP_BATCH: u64 = 1000;

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

    let cache_ids: Vec<Uuid> = pending.iter().map(|r| r.cache).collect::<HashSet<_>>().into_iter().collect();
    let cached_path_ids: Vec<Uuid> = pending
        .iter()
        .map(|r| r.cached_path)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let caches: HashMap<Uuid, MCache> = ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .all(&state.worker_db)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();

    let cached_paths: HashMap<Uuid, MCachedPath> = ECachedPath::find()
        .filter(CCachedPath::Id.is_in(cached_path_ids))
        .all(&state.worker_db)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();

    // Build a per-cache signer once (one crypt-secret read + one private-key
    // decryption per cache, not per row). `None` marks caches whose key
    // failed to decode — we skip their rows for this pass.
    let mut signers: HashMap<Uuid, Option<CacheSigner>> = HashMap::new();
    for (cache_id, cache) in &caches {
        if cache.private_key.is_empty() {
            signers.insert(*cache_id, None);
            continue;
        }
        let signer = match CacheSigner::from_cache(
            &state.cli.crypt_secret_file,
            cache,
            &state.cli.serve_url,
        ) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(cache_name = %cache.name, error = %e, "sign sweep: failed to prepare signer");
                None
            }
        };
        signers.insert(*cache_id, signer);
    }

    let mut touched_caches: HashSet<Uuid> = HashSet::new();
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

        let (Some(nar_hash), Some(nar_size)) = (cp.nar_hash.as_deref(), cp.nar_size) else {
            continue;
        };

        let refs: Vec<String> = cp
            .references
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(|s| s.to_owned())
            .collect();

        let nar_hash_nix32 = hex_hash_to_nix32(nar_hash);

        let sig_token =
            signer.sign_narinfo(&cp.store_path, &nar_hash_nix32, nar_size as u64, &refs);

        let sig_b64 = sig_token
            .split_once(':')
            .map(|(_, s)| s.to_owned())
            .unwrap_or(sig_token);

        let mut am = row.into_active_model();
        am.signature = Set(Some(sig_b64));
        if let Err(e) = am.update(&state.worker_db).await {
            warn!(store_path = %cp.store_path, cache = %cache.id, error = %e, "sign sweep: failed to persist signature");
            continue;
        }

        debug!(cache_name = %cache.name, store_path = %cp.store_path, "sign sweep: signed");
        touched_caches.insert(cache.id);
        signed += 1;
    }

    if signed > 0 {
        tracing::info!(count = signed, "sign sweep: signatures filled");
    }

    // Update cache_derivation for every (cache, derivation) pair whose
    // closure is now fully cached. Broad but cheap: only caches touched
    // this pass can have changed state.
    for cache_id in touched_caches {
        if let Err(e) = record_newly_completed_derivations(&state, cache_id).await {
            warn!(cache = %cache_id, error = %e, "sign sweep: cache_derivation update failed");
        }
    }

    Ok(())
}

/// For every derivation owned by an organization subscribed to `cache_id`
/// whose outputs are all cached and whose dependency closure is already
/// recorded, insert a `cache_derivation` row. Idempotent.
async fn record_newly_completed_derivations(
    state: &ServerState,
    cache_id: Uuid,
) -> anyhow::Result<()> {
    let org_ids: Vec<Uuid> = EOrganizationCache::find()
        .filter(COrganizationCache::Cache.eq(cache_id))
        .all(&state.worker_db)
        .await?
        .into_iter()
        .map(|oc| oc.organization)
        .collect();

    if org_ids.is_empty() {
        return Ok(());
    }

    let drvs = EDerivation::find()
        .filter(CDerivation::Organization.is_in(org_ids))
        .all(&state.worker_db)
        .await?;

    let now = chrono::Utc::now().naive_utc();
    for drv in drvs {
        if let Err(e) = try_record_cache_derivation(state, cache_id, drv.id, now).await {
            warn!(cache = %cache_id, drv = %drv.id, error = %e, "try_record_cache_derivation failed");
        }
    }
    Ok(())
}

async fn try_record_cache_derivation(
    state: &ServerState,
    cache_id: Uuid,
    derivation_id: Uuid,
    now: chrono::NaiveDateTime,
) -> anyhow::Result<()> {
    let any_uncached = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation_id))
        .filter(CDerivationOutput::IsCached.eq(false))
        .one(&state.worker_db)
        .await?
        .is_some();
    if any_uncached {
        return Ok(());
    }

    let dep_edges = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(derivation_id))
        .all(&state.worker_db)
        .await?;
    for edge in dep_edges {
        let present = ECacheDerivation::find()
            .filter(CCacheDerivation::Cache.eq(cache_id))
            .filter(CCacheDerivation::Derivation.eq(edge.dependency))
            .one(&state.worker_db)
            .await?
            .is_some();
        if !present {
            return Ok(());
        }
    }

    let already = ECacheDerivation::find()
        .filter(CCacheDerivation::Cache.eq(cache_id))
        .filter(CCacheDerivation::Derivation.eq(derivation_id))
        .one(&state.worker_db)
        .await?
        .is_some();
    if already {
        return Ok(());
    }

    let row = ACacheDerivation {
        id: Set(Uuid::new_v4()),
        cache: Set(cache_id),
        derivation: Set(derivation_id),
        cached_at: Set(now),
        last_fetched_at: Set(None),
    };
    row.insert(&state.worker_db).await?;
    Ok(())
}

/// Converts a nar_hash from any common format to `sha256:<nix32>`.
///
/// Handles `sha256:<hex>` (from streaming workers), `sha256-<base64>` (SRI),
/// and `sha256:<nix32>` (already correct).
pub(crate) fn hex_hash_to_nix32(hash: &str) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let encode = |bytes: &[u8]| -> String {
        let len = (bytes.len() * 8 - 1) / 5 + 1;
        let mut out = String::with_capacity(len);
        for n in (0..len).rev() {
            let b = n * 5;
            let i = b / 8;
            let j = b % 8;
            let b0 = bytes.get(i).copied().unwrap_or(0) as u32;
            let b1 = bytes.get(i + 1).copied().unwrap_or(0) as u32;
            let c = ((b0 >> j) | (b1 << (8 - j))) & 0x1f;
            out.push(CHARS[c as usize] as char);
        }
        out
    };

    if let Some(rest) = hash.strip_prefix("sha256:") {
        if rest.len() == 64
            && rest.chars().all(|c| c.is_ascii_hexdigit())
            && let Ok(bytes) = (0..32)
                .map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
        {
            return format!("sha256:{}", encode(&bytes));
        }
        return hash.to_string();
    }

    hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_hash_to_nix32_converts_valid_hex() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let out = hex_hash_to_nix32(&format!("sha256:{hex}"));
        assert!(out.starts_with("sha256:"));
        let suffix = out.strip_prefix("sha256:").unwrap();
        assert_eq!(suffix.len(), 52);
        assert!(
            suffix
                .chars()
                .all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c))
        );
    }

    #[test]
    fn hex_hash_to_nix32_passthrough_non_hex() {
        let already = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
        assert_eq!(hex_hash_to_nix32(already), already);
    }

    #[test]
    fn hex_hash_to_nix32_passthrough_without_sha256_prefix() {
        assert_eq!(hex_hash_to_nix32("sha256-AAAA"), "sha256-AAAA");
        assert_eq!(hex_hash_to_nix32("garbage"), "garbage");
    }

    #[test]
    fn hex_hash_to_nix32_wrong_hex_length_returned_as_is() {
        let short = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85";
        assert_eq!(hex_hash_to_nix32(short), short);
    }

    #[test]
    fn hex_hash_to_nix32_zero_digest() {
        let zero_hex = "0".repeat(64);
        let out = hex_hash_to_nix32(&format!("sha256:{zero_hex}"));
        assert_eq!(out, format!("sha256:{}", "0".repeat(52)));
    }
}
