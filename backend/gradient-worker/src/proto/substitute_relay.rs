/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Relay a build's outputs from an upstream cache straight into ours,
//! without importing into the local nix store.

use std::collections::HashMap;

use anyhow::{Context, Result};
use gradient_proto::messages::{BuildTask, CachedPath, QueryMode};
use sha2::{Digest as _, Sha256};
use tracing::debug;

use crate::proto::compression::{
    Compression, LEVEL6_WINDOW_BYTES, decompress, detect_compression, parse_nar_hash_to_bytes,
    zstd_window_size,
};
use crate::proto::job::JobUpdater;
use crate::proto::prefetch::{
    CorruptCachedNar, MissingInputs, SubstituteNotOnUpstream, download_one_presigned,
};

/// Substitute a build's outputs as a pure NAR relay: for each output, download
/// its NAR from upstream, decompress + verify, recompress to zstd, and push it
/// straight into our cache - without importing into the nix store or fetching the
/// runtime closure. The closure is mirrored separately as each of its members is
/// substituted by its own anchor; the `closure_complete` gate orders dependents.
/// Returns the output `(name, path)` pairs. Errors map to `SubstituteUnavailable`.
pub async fn relay_external_cached_outputs(
    task: &BuildTask,
    updater: &mut JobUpdater,
) -> Result<Vec<(String, String)>> {
    let outputs: Vec<(String, String)> = task
        .outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect();
    if outputs.is_empty() {
        return Ok(Vec::new());
    }
    let paths: Vec<String> = outputs.iter().map(|(_, p)| p.clone()).collect();

    // Pull = upstream availability + narinfo (URL, nar_hash, references);
    // Push = presigned PUT targets in our own cache.
    let pull: HashMap<String, CachedPath> = updater
        .query_cache(paths.clone(), QueryMode::Pull)
        .await
        .with_context(|| format!("CacheQuery Pull (substitute) for {}", task.drv_path))?
        .into_iter()
        .map(|c| (c.path.clone(), c))
        .collect();
    let push: HashMap<String, CachedPath> = updater
        .query_cache(paths.clone(), QueryMode::Push)
        .await
        .with_context(|| format!("CacheQuery Push (substitute) for {}", task.drv_path))?
        .into_iter()
        .map(|c| (c.path.clone(), c))
        .collect();

    let http = crate::http::client();
    for (_, path) in &outputs {
        // Already in our cache (push reports it cached): nothing to relay.
        if push.get(path).map(|c| c.cached).unwrap_or(false) {
            continue;
        }
        let upstream = pull
            .get(path)
            .filter(|c| c.cached && c.url.is_some())
            .ok_or_else(|| anyhow::Error::new(SubstituteNotOnUpstream(path.clone())))?;

        let (_, fetched) = download_one_presigned(http, upstream.clone())
            .await
            .with_context(|| format!("download upstream NAR for {path}"))?;
        let (compressed, meta) = fetched.ok_or_else(|| {
            // The Pull reply said cached but the GET 404'd: the same typed
            // self-heal signal as a missing prefetch input, so the server can
            // demote the stale upstream record instead of retrying forever.
            anyhow::Error::new(MissingInputs(vec![path.clone()])).context(format!(
                "upstream reported {path} but the NAR object is missing"
            ))
        })?;

        let kind = meta
            .url
            .as_deref()
            .map(detect_compression)
            .unwrap_or(Compression::Zstd);

        // Upstream references arrive as full /nix/store paths; NarUploaded wants
        // hash-name tokens.
        let references: Vec<String> = meta
            .references
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                r.strip_prefix("/nix/store/")
                    .unwrap_or(r.as_str())
                    .to_string()
            })
            .collect();

        // Pure relay: the upstream NAR is already zstd with a window at our
        // level-6 threshold and carries the file/nar metadata, so store the
        // bytes verbatim - no decompress, no recompress, no rehash.
        let verbatim = (kind == Compression::Zstd
            && zstd_window_size(&compressed).is_some_and(|w| w >= LEVEL6_WINDOW_BYTES))
        .then(|| {
            Some((
                meta.file_hash.clone()?,
                meta.nar_hash.clone()?,
                meta.nar_size?,
            ))
        })
        .flatten();

        let (bytes, cmeta) = if let Some((file_hash, nar_hash, nar_size)) = verbatim {
            let file_size = compressed.len() as u64;
            (
                compressed,
                crate::proto::nar::CompressedNarMeta {
                    file_hash,
                    file_size,
                    nar_hash,
                    nar_size,
                },
            )
        } else {
            // Weaker/absent upstream compression: decompress (verifying against
            // the upstream nar_hash) and recompress at our level-6 threshold.
            // Multi-MB CPU work, so it runs on the blocking pool.
            let claimed = meta.nar_hash.clone();
            let p = path.clone();
            tokio::task::spawn_blocking(move || {
                let raw = decompress(&compressed, kind)
                    .with_context(|| format!("{kind:?} decompress for {p}"))?;
                if let Some(claimed) = claimed.as_deref() {
                    let actual: [u8; 32] = Sha256::digest(&raw).into();
                    let want = parse_nar_hash_to_bytes(claimed)
                        .with_context(|| format!("invalid upstream nar_hash for {p}"))?;
                    if actual != want {
                        return Err(anyhow::Error::new(CorruptCachedNar(p.clone()))
                            .context(format!("upstream NAR hash mismatch for {p}")));
                    }
                }
                crate::proto::nar::compress_nar(&raw)
                    .with_context(|| format!("recompress relay NAR for {p}"))
            })
            .await
            .context("relay decompress task panicked")??
        };

        // Transport: S3-backed caches expose a presigned PUT URL; local-disk
        // caches return none and accept the bytes via direct NarPush frames.
        crate::proto::nar::upload_nar(
            &updater.job_id,
            path,
            crate::proto::nar::NarSource::Compressed {
                bytes: &bytes,
                meta: cmeta,
                references,
                deriver: meta.deriver.clone(),
                ca: upstream.ca.clone(),
            },
            crate::proto::nar::NarSink::from_upload_url(
                push.get(path).and_then(|c| c.url.as_deref()),
                &updater.nar_recv,
            ),
            &updater.writer,
        )
        .await
        .with_context(|| format!("relay-push {path} into our cache"))?;

        debug!(%path, "relayed substitute NAR into our cache");
    }

    Ok(outputs)
}
