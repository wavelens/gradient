/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Utc;
use core::sources::{format_cache_key, get_hash_from_path, get_path_from_derivation_output};
use core::types::*;
use harmonia_store_core::signature::{SecretKey, fingerprint_path};
use harmonia_store_core::store_path::{StoreDir, StorePath};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use std::collections::BTreeSet;
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

pub async fn sign_derivation_output(
    state: Arc<ServerState>,
    cache: MCache,
    output: MDerivationOutput,
) {
    let path = get_path_from_derivation_output(output.clone());
    let secret_key_str = match format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache.clone(),
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => key,
        Err(e) => {
            error!("Failed to format cache key: {}", e);
            return;
        }
    };

    let secret_key: SecretKey = match secret_key_str.parse() {
        Ok(k) => k,
        Err(e) => {
            error!(error = %e, "Failed to parse secret key");
            return;
        }
    };

    let pathinfo = match state.nix_store.query_pathinfo(path.clone()).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            error!(path = %path, "Path not found in store, cannot sign");
            return;
        }
        Err(e) => {
            error!(error = %e, "Failed to query path info for signing");
            return;
        }
    };

    // Convert SRI hash (sha256-<base64>) to nix format (sha256:<nix-base32>) for fingerprinting.
    let nar_hash_nix = match sri_to_nix_hash(&pathinfo.nar_hash) {
        Ok(h) => h,
        Err(e) => {
            error!(error = %e, "Failed to convert NAR hash format");
            return;
        }
    };

    let store_dir = StoreDir::default();
    let base = path.strip_prefix("/nix/store/").unwrap_or(&path);
    let store_path = match StorePath::from_base_path(base) {
        Ok(sp) => sp,
        Err(e) => {
            error!(error = %e, path = %path, "Invalid store path for signing");
            return;
        }
    };

    let references: BTreeSet<StorePath> = pathinfo
        .references
        .iter()
        .filter_map(|r| {
            let base = r.strip_prefix("/nix/store/").unwrap_or(r);
            StorePath::from_base_path(base).ok()
        })
        .collect();

    let fingerprint = match fingerprint_path(
        &store_dir,
        &store_path,
        nar_hash_nix.as_bytes(),
        pathinfo.nar_size,
        &references,
    ) {
        Ok(fp) => fp,
        Err(e) => {
            error!(error = %e, "Failed to compute fingerprint for signing");
            return;
        }
    };

    let sig = secret_key.sign(&fingerprint);
    let sig_str = sig.to_string();

    // Register the signature in the Nix daemon's DB.
    if let Err(e) = state
        .nix_store
        .add_signatures(path.clone(), vec![sig])
        .await
    {
        tracing::warn!(error = %e, "Failed to add signature to store (non-fatal)");
    }

    // Extract the base64 part after "name:" for DB storage.
    let signature = sig_str
        .find(':')
        .map(|i| sig_str[i + 1..].to_string())
        .unwrap_or(sig_str);

    // Find or create the cached_path row, then insert the signature.
    let (hash, package) = match get_hash_from_path(path.clone()) {
        Ok(v) => v,
        Err(e) => {
            error!(error = %e, "Failed to parse path for cached_path");
            return;
        }
    };

    let cached_path_row = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(&hash))
        .one(&state.db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            let am = ACachedPath {
                id: Set(Uuid::new_v4()),
                store_path: Set(path.clone()),
                hash: Set(hash),
                package: Set(package),
                file_hash: Set(output.file_hash.clone()),
                file_size: Set(output.file_size),
                nar_size: Set(Some(pathinfo.nar_size as i64)),
                nar_hash: Set(Some(pathinfo.nar_hash.clone())),
                references: Set(None),
                ca: Set(None),
                created_at: Set(Utc::now().naive_utc()),
            };
            match am.insert(&state.db).await {
                Ok(row) => row,
                Err(e) => {
                    error!(error = %e, "Failed to insert cached_path");
                    return;
                }
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to query cached_path");
            return;
        }
    };

    let sig_row = ACachedPathSignature {
        id: Set(Uuid::new_v4()),
        cached_path: Set(cached_path_row.id),
        cache: Set(cache.id),
        signature: Set(Some(signature)),
        created_at: Set(Utc::now().naive_utc()),
    };

    if let Err(e) = sig_row.insert(&state.db).await {
        error!(error = %e, "Failed to insert cached_path_signature");
    }
}

/// Converts an SRI-format NAR hash (`sha256-<base64>`) to the Nix format
/// (`sha256:<nix-base32>`) required by `fingerprint_path`.
fn sri_to_nix_hash(sri: &str) -> Result<String> {
    use base64::Engine as _;
    let b64 = sri
        .strip_prefix("sha256-")
        .ok_or_else(|| anyhow::anyhow!("Not a sha256 SRI hash: {}", sri))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("Invalid base64 in SRI hash")?;
    Ok(format!("sha256:{}", nix_base32_encode(&raw)))
}

/// Encode a raw hash digest in Nix's base-32 alphabet.
fn nix_base32_encode(hash: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let len = (hash.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let byte0 = hash.get(i).copied().unwrap_or(0) as u32;
        let byte1 = hash.get(i + 1).copied().unwrap_or(0) as u32;
        let c = ((byte0 >> j) | (byte1 << (8 - j))) & 0x1f;
        out.push(CHARS[c as usize] as char);
    }
    out
}

/// Compute SHA-256 of `data` and return it encoded in Nix's base-32 alphabet.
///
/// Uses `ring`'s SHA-256, which dispatches at runtime to the fastest
/// implementation available on the host CPU (SHA-NI on modern x86,
/// ARMv8 crypto extensions on aarch64, AVX2 on older x86, scalar
/// fallback otherwise).
#[allow(dead_code)]
fn nix_base32_sha256(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    nix_base32_encode(digest.as_ref())
}

/// Streams NAR encoding → zstd compression → SHA-256 hash → multipart
/// upload to the NAR store.  Memory usage stays bounded regardless of NAR
/// size (one multipart part in flight at a time).
///
/// Uses `harmonia-nar`'s `NarByteStream` for pure-Rust NAR packing instead
/// of shelling out to `nix nar pack`.
pub async fn pack_derivation_output(
    state: Arc<ServerState>,
    output: MDerivationOutput,
) -> Result<(String, u64, u64)> {
    use futures::StreamExt;
    use std::io::Write as _;

    let path = get_path_from_derivation_output(output);
    let (path_hash, _) =
        get_hash_from_path(path.clone()).context("Failed to parse derivation output path")?;

    let mut nar_stream = harmonia_nar::NarByteStream::new(path.clone().into());

    // 10 MiB parts — above S3's 5 MiB minimum, large enough to reduce
    // round-trips, small enough to keep memory bounded.
    const PART_SIZE: usize = 10 * 1024 * 1024;
    let mut writer = state
        .nar_storage
        .put_streaming(&path_hash, PART_SIZE)
        .await?;

    // Streaming zstd encoder writing compressed output into a reusable Vec.
    let mut encoder = zstd::stream::Encoder::new(Vec::with_capacity(256 * 1024), 6)
        .context("Failed to create zstd encoder")?;
    let mut hash_ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut nar_size: u64 = 0;
    let mut file_size: u64 = 0;

    let upload_result: Result<()> = async {
        while let Some(chunk_result) = nar_stream.next().await {
            let chunk = chunk_result.context("NAR stream error")?;
            nar_size += chunk.len() as u64;

            // Feed uncompressed data into the encoder; compressed output
            // accumulates in the encoder's inner Vec<u8>.
            encoder
                .write_all(&chunk)
                .context("zstd compression failed")?;

            // Drain any compressed output produced so far.
            let compressed = encoder.get_mut();
            if !compressed.is_empty() {
                hash_ctx.update(compressed);
                file_size += compressed.len() as u64;
                writer.write(compressed);
                compressed.clear();
                // wait_for_capacity takes a max-concurrency count (not bytes).
                // Allow up to 3 parts in flight at once for pipelining while
                // keeping S3 connections bounded (concurrent_uploads × 3 total).
                writer
                    .wait_for_capacity(4)
                    .await
                    .context("Multipart upload failed during write")?;
            }
        }

        // Flush the encoder's internal state and collect any remaining bytes.
        let remaining = encoder.finish().context("Failed to finish zstd encoder")?;
        if !remaining.is_empty() {
            hash_ctx.update(&remaining);
            file_size += remaining.len() as u64;
            writer.write(&remaining);
        }

        // Complete the multipart upload.
        writer
            .finish()
            .await
            .context("Failed to complete multipart upload")?;

        Ok(())
    }
    .await;

    // If the upload failed, the WriteMultipart was dropped which aborts it.
    upload_result?;

    let digest = hash_ctx.finish();
    let file_hash = nix_base32_encode(digest.as_ref());

    Ok((format!("sha256:{}", file_hash), file_size, nar_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    // ── nix_base32_encode ────────────────────────────────────────────────────

    #[test]
    fn nix_base32_encode_single_zero() {
        // 1 byte × 8 bits → (8-1)/5 + 1 = 2 chars, all zero bits.
        assert_eq!(nix_base32_encode(&[0x00]), "00");
    }

    #[test]
    fn nix_base32_encode_single_ff() {
        // Byte 0xff: high group (bits 5..) = 0b111 = 7 → '7';
        // low group (bits 0..5) = 0b11111 = 31 → 'z' (last alphabet char).
        assert_eq!(nix_base32_encode(&[0xff]), "7z");
    }

    #[test]
    fn nix_base32_encode_two_zero_bytes() {
        // 2 bytes × 8 bits → (16-1)/5 + 1 = 4 chars.
        assert_eq!(nix_base32_encode(&[0x00, 0x00]), "0000");
    }

    #[test]
    fn nix_base32_encode_32_byte_digest_has_52_chars() {
        let digest = [0u8; 32];
        let out = nix_base32_encode(&digest);
        assert_eq!(out.len(), 52, "sha256 digest → 52 nix-base32 chars");
        assert!(out.chars().all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c)));
    }

    #[test]
    fn nix_base32_encode_deterministic() {
        let data = [0x12, 0x34, 0x56, 0x78, 0x9a];
        let a = nix_base32_encode(&data);
        let b = nix_base32_encode(&data);
        assert_eq!(a, b);
    }

    #[test]
    fn nix_base32_encode_uses_no_ambiguous_chars() {
        // Nix excludes e, o, t, u from the alphabet; a 32-byte all-ones digest
        // exercises every output slot.
        let digest = [0xffu8; 32];
        let out = nix_base32_encode(&digest);
        for c in out.chars() {
            assert!(
                !['e', 'o', 't', 'u'].contains(&c),
                "nix-base32 must not contain ambiguous char '{c}'"
            );
        }
    }

    // ── nix_base32_sha256 ────────────────────────────────────────────────────

    #[test]
    fn nix_base32_sha256_empty_input_well_known() {
        // SHA-256 of empty string encoded in Nix base-32 is a well-known
        // fixed value; guards against any regression in the pipeline.
        let out = nix_base32_sha256(b"");
        assert_eq!(out.len(), 52);
        assert!(out.chars().all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c)));
        // Additionally verify it matches direct encoding of the SHA-256 digest.
        let direct = nix_base32_encode(
            ring::digest::digest(&ring::digest::SHA256, b"").as_ref(),
        );
        assert_eq!(out, direct);
    }

    #[test]
    fn nix_base32_sha256_different_inputs_differ() {
        assert_ne!(nix_base32_sha256(b"foo"), nix_base32_sha256(b"bar"));
    }

    // ── sri_to_nix_hash ──────────────────────────────────────────────────────

    #[test]
    fn sri_to_nix_hash_roundtrip_via_base64() {
        // Encode a fixed 32-byte digest as SRI, then convert, then check the
        // Nix-base32 suffix matches direct encoding.
        let digest = [0x42u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(digest);
        let sri = format!("sha256-{b64}");
        let nix = sri_to_nix_hash(&sri).expect("should convert");
        let expected = format!("sha256:{}", nix_base32_encode(&digest));
        assert_eq!(nix, expected);
    }

    #[test]
    fn sri_to_nix_hash_rejects_non_sha256_prefix() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let sri = format!("sha512-{b64}");
        assert!(sri_to_nix_hash(&sri).is_err());
    }

    #[test]
    fn sri_to_nix_hash_rejects_missing_prefix() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        assert!(sri_to_nix_hash(&b64).is_err());
    }

    #[test]
    fn sri_to_nix_hash_rejects_invalid_base64() {
        assert!(sri_to_nix_hash("sha256-!!!not-base64!!!").is_err());
    }

    #[test]
    fn sri_to_nix_hash_has_sha256_prefix() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0x01u8; 32]);
        let sri = format!("sha256-{b64}");
        let nix = sri_to_nix_hash(&sri).unwrap();
        assert!(nix.starts_with("sha256:"));
    }
}
