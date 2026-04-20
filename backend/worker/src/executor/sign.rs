/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Sign task — Ed25519-sign store paths with the cache signing key.
//!
//! The signing key is delivered by the server as a
//! [`ServerMessage::Credential { kind: CredentialKind::SigningKey }`] message.
//! It must be available in the [`CredentialStore`] before this step runs.
//!
//! Signing logic mirrors `cache/src/cacher/signing.rs` but operates on the
//! worker's local store without any DB access.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use harmonia_store_core::signature::{SecretKey, fingerprint_path};
use harmonia_store_core::store_path::{StoreDir, StorePath};
use harmonia_store_remote::DaemonStore as _;
use harmonia_utils_hash::fmt::CommonHash as _;
use proto::messages::{PathSignature, SignItem};
use tracing::{info, warn};

use crate::nix::store::{LocalNixStore, strip_store_prefix};
use crate::proto::credentials::CredentialStore;
use crate::proto::job::JobUpdater;

/// Sign every path in `store_paths` once per cache signing key delivered
/// for the job. Returns the per-path signature bundles without reporting
/// them (callers may want to embed them in a `FetchResult` instead).
///
/// Returns an empty list when no signing keys were delivered or the input
/// is empty.
pub async fn sign_paths(
    store: &LocalNixStore,
    credentials: &CredentialStore,
    store_paths: &[String],
) -> Result<Vec<PathSignature>> {
    if store_paths.is_empty() {
        return Ok(Vec::new());
    }

    let key_secrets = credentials.signing_keys();
    if key_secrets.is_empty() {
        return Ok(Vec::new());
    }

    let mut secret_keys: Vec<SecretKey> = Vec::with_capacity(key_secrets.len());
    for ks in &key_secrets {
        match ks.expose().parse::<SecretKey>() {
            Ok(k) => secret_keys.push(k),
            Err(e) => warn!(error = %e, "skipping malformed signing key credential"),
        }
    }
    if secret_keys.is_empty() {
        return Ok(Vec::new());
    }

    let store_dir = StoreDir::default();
    let mut out: Vec<PathSignature> = Vec::with_capacity(store_paths.len());

    for store_path_str in store_paths {
        let mut signatures: Vec<String> = Vec::with_capacity(secret_keys.len());
        match sign_one_path_all_keys(store, &secret_keys, &store_dir, store_path_str).await {
            Ok(sigs) => {
                for s in sigs {
                    signatures.push(s);
                }
                info!(path = %store_path_str, count = signatures.len(), "signed store path");
            }
            Err(e) => warn!(path = %store_path_str, error = %e, "failed to sign path (non-fatal)"),
        }
        out.push(PathSignature {
            store_path: store_path_str.clone(),
            signatures,
        });
    }

    Ok(out)
}

/// Sign every path and emit a `JobUpdateKind::Signed` with the results.
/// Convenience wrapper around `sign_paths` for build-job flow.
pub async fn sign_outputs(
    store: &LocalNixStore,
    credentials: &CredentialStore,
    store_paths: &[String],
    updater: &mut JobUpdater,
) -> Result<()> {
    if store_paths.is_empty() {
        return Ok(());
    }
    if credentials.signing_keys().is_empty() {
        return Ok(());
    }

    updater.report_signing()?;
    let signatures = sign_paths(store, credentials, store_paths).await?;
    if signatures.iter().any(|ps| !ps.signatures.is_empty()) {
        updater.report_signed(signatures)?;
    }
    Ok(())
}

/// Sign a single store path with every provided secret key, register all
/// signatures in the local nix-daemon, and return them in
/// `"cache-name:base64"` format (one per key).
async fn sign_one_path_all_keys(
    store: &LocalNixStore,
    secret_keys: &[SecretKey],
    store_dir: &StoreDir,
    store_path_str: &str,
) -> Result<Vec<String>> {
    let base = strip_store_prefix(store_path_str);
    let store_path = StorePath::from_base_path(base)
        .with_context(|| format!("invalid store path: {}", store_path_str))?;

    // Query path info for the NAR hash and references.
    let mut guard = store
        .pool()
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire store for sign: {}", e))?;

    let path_info = guard
        .client()
        .query_path_info(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?
        .ok_or_else(|| anyhow::anyhow!("path not in local store: {}", store_path_str))?;

    // Convert SRI hash to Nix format for fingerprinting.
    let nar_hash_nix =
        sri_to_nix_hash(&path_info.nar_hash.sri().to_string()).context("convert NAR hash")?;

    let references: BTreeSet<StorePath> = path_info
        .references
        .iter()
        .filter_map(|r| StorePath::from_base_path(strip_store_prefix(&r.to_string())).ok())
        .collect();

    let fingerprint = fingerprint_path(
        store_dir,
        &store_path,
        nar_hash_nix.as_bytes(),
        path_info.nar_size,
        &references,
    )
    .context("compute fingerprint")?;

    let mut sig_strings: Vec<String> = Vec::with_capacity(secret_keys.len());
    let mut sigs_for_daemon = Vec::with_capacity(secret_keys.len());
    for key in secret_keys {
        let sig = key.sign(&fingerprint);
        sig_strings.push(sig.to_string());
        sigs_for_daemon.push(sig);
    }

    guard
        .client()
        .add_signatures(&store_path, &sigs_for_daemon)
        .await
        .map_err(|e| anyhow::anyhow!("add_signatures failed: {}", e))?;

    Ok(sig_strings)
}

/// Sign a batch of [`SignItem`]s purely from metadata (no NAR access).
///
/// Used by the `SignJob` handler: the server already has every path's
/// `store_path`, `nar_hash`, `nar_size`, and `references` on
/// `cached_path`, so the worker just rebuilds the narinfo fingerprint
/// locally and signs once per delivered key. Reports via
/// `JobUpdateKind::Signed`.
pub async fn sign_items(
    credentials: &CredentialStore,
    items: &[SignItem],
    updater: &mut JobUpdater,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    let key_secrets = credentials.signing_keys();
    if key_secrets.is_empty() {
        warn!("SignJob delivered without any signing-key credentials; nothing to do");
        return Ok(());
    }

    let mut secret_keys: Vec<SecretKey> = Vec::with_capacity(key_secrets.len());
    for ks in &key_secrets {
        match ks.expose().parse::<SecretKey>() {
            Ok(k) => secret_keys.push(k),
            Err(e) => warn!(error = %e, "skipping malformed signing key credential"),
        }
    }
    if secret_keys.is_empty() {
        return Ok(());
    }

    updater.report_signing()?;

    let store_dir = StoreDir::default();
    let mut out: Vec<PathSignature> = Vec::with_capacity(items.len());

    for item in items {
        match sign_one_item(&store_dir, &secret_keys, item) {
            Ok(sigs) => out.push(PathSignature {
                store_path: item.store_path.clone(),
                signatures: sigs,
            }),
            Err(e) => {
                warn!(path = %item.store_path, error = %e, "failed to sign sign-item (skipping)");
                out.push(PathSignature {
                    store_path: item.store_path.clone(),
                    signatures: Vec::new(),
                });
            }
        }
    }

    if out.iter().any(|ps| !ps.signatures.is_empty()) {
        updater.report_signed(out)?;
    }
    Ok(())
}

/// Build the narinfo fingerprint from a [`SignItem`] and sign once per key.
fn sign_one_item(
    store_dir: &StoreDir,
    secret_keys: &[SecretKey],
    item: &SignItem,
) -> Result<Vec<String>> {
    let base = strip_store_prefix(&item.store_path);
    let store_path = StorePath::from_base_path(base)
        .with_context(|| format!("invalid store path: {}", item.store_path))?;

    // References: bare hash-name → StorePath. Unparseable entries are
    // skipped with a warning (the server should never send them).
    let references: BTreeSet<StorePath> = item
        .references
        .iter()
        .filter_map(|r| match StorePath::from_base_path(strip_store_prefix(r)) {
            Ok(sp) => Some(sp),
            Err(e) => {
                warn!(store_path = %item.store_path, reference = %r, error = %e, "unparseable reference in SignItem");
                None
            }
        })
        .collect();

    let fingerprint = fingerprint_path(
        store_dir,
        &store_path,
        item.nar_hash.as_bytes(),
        item.nar_size,
        &references,
    )
    .context("compute fingerprint from SignItem")?;

    let mut out = Vec::with_capacity(secret_keys.len());
    for key in secret_keys {
        out.push(key.sign(&fingerprint).to_string());
    }
    Ok(out)
}

/// Converts an SRI-format NAR hash (`sha256-<base64>`) to the Nix format
/// (`sha256:<nix-base32>`) required by `fingerprint_path`.
fn sri_to_nix_hash(sri: &str) -> Result<String> {
    use base64::Engine as _;
    let b64 = sri
        .strip_prefix("sha256-")
        .ok_or_else(|| anyhow::anyhow!("not a sha256 SRI hash: {}", sri))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("invalid base64 in SRI hash")?;
    Ok(format!("sha256:{}", nix_base32_encode(&raw)))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_base32_encode_zeros() {
        // 32 zero bytes → 52 '0' characters (each 5-bit group is 0).
        let result = nix_base32_encode(&[0u8; 32]);
        assert_eq!(result.len(), 52);
        assert!(
            result.chars().all(|c| c == '0'),
            "expected all zeros, got {result}"
        );
    }

    #[test]
    fn nix_base32_encode_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb924...
        // Expected nix-base32 verified by running the nix_base32_encode function itself.
        let empty_sha256: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        let result = nix_base32_encode(&empty_sha256);
        assert_eq!(
            result,
            "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"
        );
        assert_eq!(result.len(), 52);
    }

    #[test]
    fn sri_to_nix_hash_valid() {
        use base64::Engine as _;
        let empty_sha256: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        let b64 = base64::engine::general_purpose::STANDARD.encode(empty_sha256);
        let sri = format!("sha256-{b64}");
        let result = sri_to_nix_hash(&sri).unwrap();
        assert_eq!(
            result,
            "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"
        );
    }

    #[test]
    fn sri_to_nix_hash_rejects_non_sha256() {
        let err = sri_to_nix_hash("md5-AAAA").unwrap_err();
        assert!(
            err.to_string().contains("sha256"),
            "unexpected error: {err}"
        );
    }

    /// `sign_one_item` rejects a SignItem with a malformed `store_path`.
    /// Protects against bad server data silently producing empty signatures.
    #[test]
    fn sign_one_item_rejects_invalid_store_path() {
        let store_dir = StoreDir::default();
        let item = SignItem {
            store_path: "not-a-store-path".into(),
            nar_hash: "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into(),
            nar_size: 0,
            references: vec![],
        };
        let err = sign_one_item(&store_dir, &[], &item).expect_err("invalid path must error");
        assert!(
            format!("{err:#}").contains("invalid store path"),
            "unexpected error: {err:#}"
        );
    }
}
