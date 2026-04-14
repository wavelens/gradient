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
use proto::messages::SignTask;
use tracing::{info, warn};

use crate::nix::store::{LocalNixStore, strip_store_prefix};
use crate::proto::credentials::CredentialStore;
use crate::proto::job::JobUpdater;

/// Sign all store paths in `task` with the cache signing key from `credentials`.
///
/// Fails with an error if no signing key credential has been received.
pub async fn sign_outputs(
    store: &LocalNixStore,
    credentials: &CredentialStore,
    task: &SignTask,
    updater: &mut JobUpdater<'_>,
) -> Result<()> {
    updater.report_signing().await?;

    let key_secret = credentials
        .signing_key()
        .ok_or_else(|| anyhow::anyhow!("no signing key credential received for this job"))?;

    let secret_key: SecretKey = key_secret
        .expose()
        .parse()
        .map_err(|e| anyhow::anyhow!("failed to parse signing key: {}", e))?;

    let store_dir = StoreDir::default();

    for store_path_str in &task.store_paths {
        match sign_one_path(store, &secret_key, &store_dir, store_path_str).await {
            Ok(()) => info!(path = %store_path_str, "signed store path"),
            Err(e) => warn!(path = %store_path_str, error = %e, "failed to sign path (non-fatal)"),
        }
    }

    Ok(())
}

/// Sign a single store path and add the signature to the local nix-daemon.
async fn sign_one_path(
    store: &LocalNixStore,
    secret_key: &SecretKey,
    store_dir: &StoreDir,
    store_path_str: &str,
) -> Result<()> {
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

    let sig = secret_key.sign(&fingerprint);
    guard
        .client()
        .add_signatures(&store_path, &[sig])
        .await
        .map_err(|e| anyhow::anyhow!("add_signatures failed: {}", e))?;

    Ok(())
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
}
