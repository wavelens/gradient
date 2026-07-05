/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Stream a server-supplied NAR straight into the local nix-daemon.
//!
//! Receives the (still zstd-compressed) NAR bytes plus the cache metadata
//! that came back in `CacheStatus`, decompresses in-memory, constructs a
//! [`ValidPathInfo`], and calls harmonia's `add_to_store_nar` over the
//! daemon socket. No on-disk staging, no `nix copy` subprocess, no
//! signature/key configuration on the worker - the WS transport itself is
//! authenticated, so we pass `dont_check_sigs: true`.

use std::pin::pin;

use anyhow::{Context, Result};
use futures::stream::StreamExt as _;
use gradient_proto::messages::CachedPath;
use harmonia_protocol::valid_path_info::ValidPathInfo;
use harmonia_store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use sha2::{Digest as _, Sha256};
use tracing::debug;

use crate::nix::store::LocalNixStore;
use crate::proto::compression::{
    Compression, build_unkeyed_path_info, decompress, detect_compression, parse_nar_hash_to_bytes,
};
use crate::proto::prefetch::CorruptCachedNar;

// ── NarImporter ───────────────────────────────────────────────────────────────

/// Decompresses a single server-supplied NAR, verifies its integrity, builds
/// the [`ValidPathInfo`] the daemon expects, and streams it via
/// `add_to_store_nar`. Created by [`import_received_nar`].
struct NarImporter<'a> {
    store: &'a LocalNixStore,
    store_path: &'a str,
    meta: &'a CachedPath,
}

impl<'a> NarImporter<'a> {
    fn new(store: &'a LocalNixStore, store_path: &'a str, meta: &'a CachedPath) -> Self {
        Self {
            store,
            store_path,
            meta,
        }
    }

    fn build_path_info(&self, nar_size: u64) -> Result<ValidPathInfo> {
        let path_base = self
            .store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(self.store_path);

        let path = StorePath::from_base_path(path_base)
            .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", self.store_path, e))?;

        let info = build_unkeyed_path_info(self.store_path, self.meta, nar_size)?;
        Ok(ValidPathInfo { path, info })
    }

    async fn stream_to_daemon(
        &self,
        decompressed: &[u8],
        valid_info: &ValidPathInfo,
    ) -> Result<()> {
        let mut guard = self.store.acquire().await?;

        guard
            .execute(|client| async move {
                let logs = client.add_to_store_nar(
                    valid_info,
                    decompressed,
                    false, // repair
                    true,  // dont_check_sigs - we trust the authenticated WS transport
                );
                let mut logs = pin!(logs);
                while let Some(_msg) = logs.next().await {}
                logs.await
            })
            .await
            .map_err(|e| {
                anyhow::anyhow!("daemon add_to_store_nar({}) failed: {}", self.store_path, e)
            })
    }

    async fn import(&self, compressed_nar: Vec<u8>) -> Result<()> {
        // Compression is inferred from the `URL:` field in the narinfo that
        // was rewritten into `meta.url`. When the bytes came in via
        // `NarRequest` (WebSocket, no URL), we default to zstd - that's the
        // only format our own cache ever produces. Decompress + digest are
        // multi-MB CPU work, so both run on the blocking pool.
        let kind = self
            .meta
            .url
            .as_deref()
            .map(detect_compression)
            .unwrap_or(Compression::Zstd);
        let store_path = self.store_path.to_owned();
        let expected_size = self.meta.nar_size;
        let claimed_hash = self.meta.nar_hash.clone();
        let compressed_len = compressed_nar.len();
        let decompressed = tokio::task::spawn_blocking(move || {
            let raw = decompress(&compressed_nar, kind)
                .with_context(|| format!("{kind:?} decompress failed for {store_path}"))?;
            verify_nar(&store_path, &raw, expected_size, claimed_hash.as_deref())?;
            Ok::<_, anyhow::Error>(raw)
        })
        .await
        .context("decompress task panicked")??;
        let valid_info = self.build_path_info(decompressed.len() as u64)?;
        self.stream_to_daemon(&decompressed, &valid_info).await?;
        debug!(%self.store_path, bytes = compressed_len, "imported NAR into local store");
        Ok(())
    }
}

/// Check a decompressed NAR against the size and `sha256:` hash its metadata
/// claims. A mismatch is a typed [`CorruptCachedNar`] so the executor can
/// route it into the demote-and-refetch self-heal instead of a retry loop.
fn verify_nar(
    store_path: &str,
    decompressed: &[u8],
    expected_size: Option<u64>,
    claimed_nar_hash: Option<&str>,
) -> Result<()> {
    if let Some(expected) = expected_size
        && decompressed.len() as u64 != expected
    {
        return Err(
            anyhow::Error::new(CorruptCachedNar(store_path.to_owned())).context(format!(
                "NAR size mismatch for {}: expected {}, got {}",
                store_path,
                expected,
                decompressed.len()
            )),
        );
    }

    if let Some(claimed_nar_hash) = claimed_nar_hash {
        let actual_nar_hash: [u8; 32] = Sha256::digest(decompressed).into();
        let claimed = parse_nar_hash_to_bytes(claimed_nar_hash)
            .with_context(|| format!("invalid nar_hash for {store_path}"))?;

        if actual_nar_hash != claimed {
            return Err(
                anyhow::Error::new(CorruptCachedNar(store_path.to_owned())).context(format!(
                    "NAR hash mismatch for {}: server said {}, computed {}",
                    store_path,
                    claimed_nar_hash,
                    crate::proto::nar::sha256_nix32(decompressed)
                )),
            );
        }
    }

    Ok(())
}

// ── Public import entry point ─────────────────────────────────────────────────

/// Decompress + import a single NAR delivered via `NarPush` (or downloaded
/// from a presigned URL) into the worker's local nix-daemon.
pub async fn import_received_nar(
    store: &LocalNixStore,
    store_path: &str,
    compressed_nar: Vec<u8>,
    meta: &CachedPath,
) -> Result<()> {
    NarImporter::new(store, store_path, meta)
        .import(compressed_nar)
        .await
}
