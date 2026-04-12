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
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_core::signature::{SecretKey, fingerprint_path};
use harmonia_utils_hash::fmt::CommonHash as _;
use harmonia_store_core::store_path::{StoreDir, StorePath};
use proto::messages::SignTask;
use tracing::{info, warn};

use crate::credentials::CredentialStore;
use crate::job::JobUpdater;
use crate::store::{LocalNixStore, strip_store_prefix};

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
    let store_path =
        StorePath::from_base_path(base).with_context(|| format!("invalid store path: {}", store_path_str))?;

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
