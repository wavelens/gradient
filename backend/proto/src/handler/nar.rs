/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Timelike;
use gradient_core::types::*;
use gradient_core::types::proto::PathSignature;
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

/// Write one `cached_path_signature` row per entry in `signatures`. Each
/// entry is `"<cache-name>:<base64>"`; the cache name is resolved against
/// the job's org caches. Foreign or unknown cache names are logged and
/// dropped. Entries whose cache already has a signature for this path are
/// skipped.
async fn record_signatures_by_cache_name(
    state: &ServerState,
    cached_path_id: Uuid,
    org_caches: &[entity::organization_cache::Model],
    signatures: &[String],
    store_path: &str,
    now: chrono::NaiveDateTime,
) {
    if signatures.is_empty() {
        return;
    }

    // Pre-resolve the org's cache rows so we can match by name.
    let cache_ids: Vec<Uuid> = org_caches.iter().map(|oc| oc.cache).collect();
    let caches = match ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .all(&state.db)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to resolve org caches for signature recording");
            return;
        }
    };

    for sig in signatures {
        let (cache_name, sig_b64) = match split_signature(sig) {
            Some(v) => v,
            None => {
                warn!(%store_path, sig = %sig, "malformed signature entry, skipping");
                continue;
            }
        };

        let Some(cache) = caches.iter().find(|c| c.name == cache_name) else {
            warn!(%store_path, %cache_name, "signature for cache not owned by job's org, dropping");
            continue;
        };

        let existing = ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cached_path_id))
            .filter(CCachedPathSignature::Cache.eq(cache.id))
            .one(&state.db)
            .await
            .unwrap_or(None);
        if existing.is_some() {
            continue;
        }

        let sig_row = ACachedPathSignature {
            id: Set(Uuid::new_v4()),
            cached_path: Set(cached_path_id),
            cache: Set(cache.id),
            signature: Set(Some(sig_b64.to_string())),
            created_at: Set(now),
        };
        if let Err(e) = sig_row.insert(&state.db).await {
            warn!(
                %store_path,
                cache = %cache.id,
                error = %e,
                "failed to insert cached_path_signature"
            );
        }
    }
}

/// Split a narinfo signature `"<name>:<base64>"` into `(name, base64)`.
/// Returns `None` if there is no `:` separator.
fn split_signature(sig: &str) -> Option<(&str, &str)> {
    let (name, rest) = sig.split_once(':')?;
    Some((name, rest))
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

    let hash_name = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path);
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

    // Signatures are reported separately via `JobUpdateKind::Signed` (see
    // `record_worker_signatures`). Server no longer signs anything.
    let _ = scheduler;
    let _ = job_id;
    let _ = cached_path_row;

    info!(store_path, "cached_path metadata recorded after NarUploaded");
    Ok(())
}

/// Store per-path signatures reported by the worker's Sign task.
///
/// For each signature, finds the `cached_path` row and upserts a
/// `cached_path_signature` row for every cache the org subscribes to.
/// The signature string is expected in `"key-name:base64"` format; only the
/// base64 portion is stored (the key name is reconstructed from the cache and
/// serve URL at narinfo read time).
pub(super) async fn record_worker_signatures(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    signatures: &[PathSignature],
) -> anyhow::Result<()> {
    if signatures.is_empty() {
        return Ok(());
    }

    let Some(org_id) = scheduler.peer_id_for_job(job_id).await else {
        return Ok(());
    };

    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .all(&state.db)
        .await?;

    if org_caches.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().naive_utc();

    for ps in signatures {
        let hash = ps
            .store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(&ps.store_path)
            .split('-')
            .next()
            .unwrap_or("");

        if hash.is_empty() {
            warn!(store_path = %ps.store_path, "Signed: could not parse hash from store path");
            continue;
        }

        let cached_path_row = match ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash))
            .one(&state.db)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                warn!(store_path = %ps.store_path, "Signed: no cached_path row found");
                continue;
            }
            Err(e) => {
                warn!(store_path = %ps.store_path, error = %e, "Signed: cached_path lookup failed");
                continue;
            }
        };

        record_signatures_by_cache_name(
            state,
            cached_path_row.id,
            &org_caches,
            &ps.signatures,
            &ps.store_path,
            now,
        )
        .await;

        // If this path corresponds to a derivation_output, check whether
        // its derivation's closure is now fully cached+signed in any cache
        // and record `cache_derivation` accordingly.
        if let Ok(Some(output)) = EDerivationOutput::find()
            .filter(CDerivationOutput::Hash.eq(hash))
            .one(&state.db)
            .await
        {
            for oc in &org_caches {
                if let Err(e) =
                    try_record_cache_derivation(state, oc.cache, output.derivation, now).await
                {
                    warn!(cache = %oc.cache, drv = %output.derivation, error = %e, "try_record_cache_derivation failed");
                }
            }
        }
    }

    info!(count = signatures.len(), %org_id, "recorded worker signatures");
    Ok(())
}

/// If every output of `derivation_id` is cached AND every transitive
/// dependency already has a `cache_derivation` row for `cache_id`, insert
/// the row. Idempotent.
async fn try_record_cache_derivation(
    state: &ServerState,
    cache_id: Uuid,
    derivation_id: Uuid,
    now: chrono::NaiveDateTime,
) -> anyhow::Result<()> {
    let any_uncached = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation_id))
        .filter(CDerivationOutput::IsCached.eq(false))
        .one(&state.db)
        .await?
        .is_some();
    if any_uncached {
        return Ok(());
    }

    let dep_edges = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(derivation_id))
        .all(&state.db)
        .await?;
    for edge in dep_edges {
        let present = ECacheDerivation::find()
            .filter(CCacheDerivation::Cache.eq(cache_id))
            .filter(CCacheDerivation::Derivation.eq(edge.dependency))
            .one(&state.db)
            .await?
            .is_some();
        if !present {
            return Ok(());
        }
    }

    let already = ECacheDerivation::find()
        .filter(CCacheDerivation::Cache.eq(cache_id))
        .filter(CCacheDerivation::Derivation.eq(derivation_id))
        .one(&state.db)
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
    row.insert(&state.db).await?;
    Ok(())
}

/// Converts a nar_hash from any common format to `sha256:<nix32>`.
///
/// Handles `sha256:<hex>` (from streaming workers), `sha256-<base64>` (SRI),
/// and `sha256:<nix32>` (already correct).
#[cfg(test)]
fn hex_hash_to_nix32(hash: &str) -> String {
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

    // sha256:<hex> (64 hex chars)
    if let Some(rest) = hash.strip_prefix("sha256:") {
        if rest.len() == 64 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(bytes) = (0..32)
                .map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
            {
                return format!("sha256:{}", encode(&bytes));
            }
        }
        // Already nix32 (or unknown) — return as-is.
        return hash.to_string();
    }

    hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_hash_to_nix32_converts_valid_hex() {
        // SHA-256 of empty string: e3b0c442 98fc1c14 ...
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let out = hex_hash_to_nix32(&format!("sha256:{hex}"));
        assert!(out.starts_with("sha256:"));
        let suffix = out.strip_prefix("sha256:").unwrap();
        assert_eq!(suffix.len(), 52, "nix32-encoded sha256 is 52 chars");
        assert!(
            suffix.chars().all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c)),
            "nix32 alphabet only: {suffix}"
        );
    }

    #[test]
    fn hex_hash_to_nix32_passthrough_non_hex() {
        // Already nix32 (52 chars) passes through unchanged.
        let already = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
        assert_eq!(hex_hash_to_nix32(already), already);
    }

    #[test]
    fn hex_hash_to_nix32_passthrough_without_sha256_prefix() {
        // No sha256: prefix → return unchanged (e.g. SRI sha256-<b64>).
        assert_eq!(hex_hash_to_nix32("sha256-AAAA"), "sha256-AAAA");
        assert_eq!(hex_hash_to_nix32("garbage"), "garbage");
    }

    #[test]
    fn hex_hash_to_nix32_wrong_hex_length_returned_as_is() {
        // 63 hex chars — invalid, must be returned unchanged.
        let short = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85";
        assert_eq!(hex_hash_to_nix32(short), short);
    }

    #[test]
    fn hex_hash_to_nix32_non_hex_chars_returned_as_is() {
        // Contains 'z' — not a hex char.
        let bad = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85z";
        assert_eq!(hex_hash_to_nix32(bad), bad);
    }

    #[test]
    fn hex_hash_to_nix32_accepts_uppercase_hex() {
        let lower = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let upper = lower.to_ascii_uppercase();
        // is_ascii_hexdigit accepts both cases; output must match.
        let out_lower = hex_hash_to_nix32(&format!("sha256:{lower}"));
        let out_upper = hex_hash_to_nix32(&format!("sha256:{upper}"));
        assert_eq!(out_lower, out_upper);
    }

    #[test]
    fn hex_hash_to_nix32_zero_digest() {
        // 32 zero bytes → 52 zero chars (all 5-bit groups are 0 → '0').
        let zero_hex = "0".repeat(64);
        let out = hex_hash_to_nix32(&format!("sha256:{zero_hex}"));
        assert_eq!(out, format!("sha256:{}", "0".repeat(52)));
    }

    #[test]
    fn hex_hash_to_nix32_ff_digest_last_char_is_z() {
        // The final output char (reverse-emitted, so emitted last in the
        // loop; n=0) reads bits 0..5 of byte 0. For 0xff this is 0b11111 = 31
        // → 'z' (last char of the nix-base32 alphabet).
        let ff_hex = "ff".repeat(32);
        let out = hex_hash_to_nix32(&format!("sha256:{ff_hex}"));
        let suffix = out.strip_prefix("sha256:").unwrap();
        assert!(suffix.ends_with('z'), "expected trailing 'z', got: {suffix}");
        assert_eq!(suffix.len(), 52);
    }

    #[test]
    fn hex_hash_to_nix32_deterministic_different_inputs_differ() {
        let a = hex_hash_to_nix32(
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        let b = hex_hash_to_nix32(
            "sha256:f3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        assert_ne!(a, b);
    }
}
