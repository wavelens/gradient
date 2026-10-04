/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read as _, Seek as _};

use anyhow::{Context, Result};
use gradient_wire::messages::CachedPath;
use harmonia_protocol::valid_path_info::ValidPathInfo;
use harmonia_store_path::StorePath;
use sha2::{Digest as _, Sha256};
use tracing::{debug, warn};

use crate::nix::store::LocalNixStore;
use crate::proto::compression::build_unkeyed_path_info;
use crate::proto::prefetch::CorruptCachedNar;
use gradient_worker_client::compression::{
    SNIFF_BYTES, decompress, decompress_reader, parse_nar_hash_to_bytes, resolve_compression,
};
use gradient_worker_client::nar_recv::NarPayload;

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

    fn build_path_info(&self, meta: &CachedPath, nar_size: u64) -> Result<ValidPathInfo> {
        let path_base = self
            .store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(self.store_path);

        let path = StorePath::from_base_path(path_base)
            .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", self.store_path, e))?;

        let info = build_unkeyed_path_info(self.store_path, meta, nar_size)?;
        Ok(ValidPathInfo { path, info })
    }

    async fn import(&self, payload: NarPayload) -> Result<()> {
        // Compression is detected from the payload magic bytes, then the narinfo `URL:` field, then
        // zstd. A `NarRequest` over the WebSocket is only carrying zstd.
        let store_path = self.store_path.to_owned();
        let expected_size = self.meta.nar_size;
        let claimed_hash = self.meta.nar_hash.clone();
        let url = self.meta.url.clone();
        let (decompressed, compressed_len) = tokio::task::spawn_blocking(move || {
            let (raw, compressed_len) = match payload {
                NarPayload::Bytes(compressed) => {
                    let kind = resolve_compression(&compressed, url.as_deref());
                    let raw = decompress(&compressed, kind)
                        .with_context(|| format!("{kind:?} decompress failed for {store_path}"))?;
                    (raw, compressed.len() as u64)
                }
                NarPayload::File(path) => {
                    let mut file = std::fs::File::open(&path)
                        .with_context(|| format!("open staged NAR {}", path.display()))?;
                    let mut magic = Vec::with_capacity(SNIFF_BYTES);
                    file.by_ref()
                        .take(SNIFF_BYTES as u64)
                        .read_to_end(&mut magic)
                        .with_context(|| format!("read staged NAR {}", path.display()))?;
                    let kind = resolve_compression(&magic, url.as_deref());
                    file.rewind().context("rewind staged NAR")?;
                    let compressed_len = file.metadata().context("stat staged NAR")?.len();
                    let raw = decompress_reader(&mut file, kind)
                        .with_context(|| format!("{kind:?} decompress failed for {store_path}"))?;
                    drop(file);
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!(path = %path.display(), error = %e, "could not remove staged NAR");
                    }
                    (raw, compressed_len)
                }
            };
            verify_nar(&store_path, &raw, expected_size, claimed_hash.as_deref())?;
            Ok::<_, anyhow::Error>((raw, compressed_len))
        })
        .await
        .context("decompress task panicked")??;
        let meta = with_nar_hash(self.meta, &decompressed);
        let valid_info = self.build_path_info(&meta, decompressed.len() as u64)?;
        self.store.import_nar(&valid_info, &decompressed).await?;
        debug!(%self.store_path, bytes = compressed_len, "imported NAR into local store");
        Ok(())
    }
}

fn with_nar_hash(meta: &CachedPath, nar: &[u8]) -> CachedPath {
    let mut meta = meta.clone();
    meta.nar_hash
        .get_or_insert_with(|| gradient_worker_client::nar::sha256_nix32(nar));
    meta
}

/// A mismatch is a typed [`CorruptCachedNar`]. The executor is routing it into demote-and-refetch
/// instead of a retry loop.
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
                    gradient_worker_client::nar::sha256_nix32(decompressed)
                )),
            );
        }
    }

    Ok(())
}

pub async fn import_received_nar(
    store: &LocalNixStore,
    store_path: &str,
    payload: NarPayload,
    meta: &CachedPath,
) -> Result<()> {
    NarImporter::new(store, store_path, meta)
        .import(payload)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nar_without_a_claimed_hash_is_registered_under_the_hash_of_its_bytes() {
        let meta = CachedPath {
            path: "/nix/store/aaaa-hello".into(),
            ..CachedPath::default()
        };
        let filled = with_nar_hash(&meta, b"nar bytes");
        assert_eq!(
            filled.nar_hash.as_deref(),
            Some(gradient_worker_client::nar::sha256_nix32(b"nar bytes").as_str())
        );
    }

    #[test]
    fn a_claimed_hash_is_kept() {
        let meta = CachedPath {
            nar_hash: Some("sha256:claimed".into()),
            ..CachedPath::default()
        };
        assert_eq!(
            with_nar_hash(&meta, b"nar bytes").nar_hash.as_deref(),
            Some("sha256:claimed")
        );
    }
}
