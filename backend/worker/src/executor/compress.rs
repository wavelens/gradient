/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Compress task — dump store paths to NAR and compress with zstd before upload.
//!
//! Uses `harmonia-nar`'s `NarByteStream` for pure-Rust NAR packing (no `nix nar`
//! subprocess). The compressed data is pushed to the server in 64 KiB chunks via
//! [`ClientMessage::NarPush`].

use std::io::Write as _;

use anyhow::{Context, Result};
use futures::StreamExt;
use proto::messages::{ClientMessage, CompressTask};
use tracing::{debug, info};

use crate::job::JobUpdater;
use crate::store::LocalNixStore;

/// Chunk size for streamed NarPush (64 KiB).
const CHUNK_SIZE: usize = 64 * 1024;

/// Compress all store paths in `task` into zstd-compressed NARs and push them
/// to the server via direct WebSocket transfer.
///
/// The server receives the chunks and imports them into its local store.
pub async fn compress_outputs(
    _store: &LocalNixStore,
    task: &CompressTask,
    updater: &mut JobUpdater<'_>,
) -> Result<()> {
    updater.report_compressing().await?;

    for store_path in &task.store_paths {
        compress_and_push(store_path, updater).await?;
    }

    Ok(())
}

/// Pack a single store path into a zstd-compressed NAR and push it to the server.
async fn compress_and_push(store_path: &str, updater: &mut JobUpdater<'_>) -> Result<()> {
    debug!(store_path, "compressing NAR");

    let mut nar_stream = harmonia_nar::NarByteStream::new(store_path.to_owned().into());

    // Streaming zstd encoder: compress the NAR on-the-fly.
    let mut encoder = zstd::stream::Encoder::new(Vec::with_capacity(CHUNK_SIZE * 2), 6)
        .context("failed to create zstd encoder")?;

    let mut offset: u64 = 0;

    // Drain the NAR stream, compress, and push in CHUNK_SIZE pieces.
    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        encoder
            .write_all(&chunk)
            .context("zstd compression failed")?;

        let compressed = encoder.get_mut();
        while compressed.len() >= CHUNK_SIZE {
            let part: Vec<u8> = compressed.drain(..CHUNK_SIZE).collect();
            let part_len = part.len() as u64;
            updater
                .conn
                .send(ClientMessage::NarPush {
                    job_id: updater.job_id.clone(),
                    store_path: store_path.to_owned(),
                    data: part,
                    offset,
                    is_final: false,
                })
                .await?;
            offset += part_len;
        }
    }

    // Flush remaining compressed bytes.
    let remaining = encoder.finish().context("failed to finish zstd encoder")?;

    if !remaining.is_empty() {
        let remaining_len = remaining.len() as u64;
        updater
            .conn
            .send(ClientMessage::NarPush {
                job_id: updater.job_id.clone(),
                store_path: store_path.to_owned(),
                data: remaining,
                offset,
                is_final: false,
            })
            .await?;
        offset += remaining_len;
    }

    // Send final empty chunk to signal completion of this path.
    updater
        .conn
        .send(ClientMessage::NarPush {
            job_id: updater.job_id.clone(),
            store_path: store_path.to_owned(),
            data: vec![],
            offset,
            is_final: true,
        })
        .await?;

    info!(store_path, compressed_bytes = offset, "NAR push complete");
    Ok(())
}
