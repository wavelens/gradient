/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! NAR transfer — send built store paths to the server.
//!
//! Two modes depending on server configuration:
//! - **Direct**: chunked [`ClientMessage::NarPush`] frames over the WebSocket
//!   (zstd-compressed, 64 KiB chunks).  Initiated by the worker after a build;
//!   this mirrors [`crate::executor::compress`] but is triggered by a server
//!   `PresignedUpload` message that includes no URL (direct mode sentinel).
//! - **S3**: server sends a [`ServerMessage::PresignedUpload`] with a URL;
//!   worker compresses the NAR, HTTP-PUTs it to S3, then confirms with
//!   [`ClientMessage::NarReady`].

use std::io::Write as _;

use anyhow::{Context, Result};
use futures::StreamExt;
use proto::messages::ClientMessage;
use sha2::{Digest, Sha256};
use tracing::{debug, info};

use crate::connection::ProtoConnection;

/// Chunk size for direct NAR streaming (64 KiB).
const NAR_CHUNK_SIZE: usize = 64 * 1024;

/// Compress `store_path` into a zstd-compressed NAR and push it to the server
/// in [`NAR_CHUNK_SIZE`]-byte chunks via [`ClientMessage::NarPush`].
///
/// This is the "direct" transfer mode — no S3 involved.
pub async fn push_direct(
    job_id: &str,
    store_path: &str,
    conn: &mut ProtoConnection,
) -> Result<()> {
    debug!(store_path, "NAR direct push");

    let mut nar_stream = harmonia_nar::NarByteStream::new(store_path.to_owned().into());
    let mut encoder = zstd::stream::Encoder::new(Vec::with_capacity(NAR_CHUNK_SIZE * 2), 6)
        .context("failed to create zstd encoder")?;
    let mut offset: u64 = 0;

    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        encoder.write_all(&chunk).context("zstd compression failed")?;

        let buf = encoder.get_mut();
        while buf.len() >= NAR_CHUNK_SIZE {
            let part: Vec<u8> = buf.drain(..NAR_CHUNK_SIZE).collect();
            let len = part.len() as u64;
            conn.send(ClientMessage::NarPush {
                job_id: job_id.to_owned(),
                store_path: store_path.to_owned(),
                data: part,
                offset,
                is_final: false,
            })
            .await?;
            offset += len;
        }
    }

    let remaining = encoder.finish().context("failed to finish zstd encoder")?;
    if !remaining.is_empty() {
        let len = remaining.len() as u64;
        conn.send(ClientMessage::NarPush {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            data: remaining,
            offset,
            is_final: false,
        })
        .await?;
        offset += len;
    }

    // Empty final chunk signals end-of-path.
    conn.send(ClientMessage::NarPush {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        data: vec![],
        offset,
        is_final: true,
    })
    .await?;

    info!(store_path, compressed_bytes = offset, "direct NAR push complete");
    Ok(())
}

/// Upload `store_path` as a zstd-compressed NAR to the presigned `url`, then
/// send [`ClientMessage::NarReady`] with the compressed size and SHA-256 hash.
///
/// `method` is the HTTP method the server expects (usually `"PUT"`).
/// `headers` are additional HTTP headers to include (e.g. `x-amz-*` for S3).
pub async fn upload_presigned(
    job_id: &str,
    store_path: &str,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    conn: &mut ProtoConnection,
) -> Result<()> {
    debug!(store_path, method, "presigned NAR upload");

    // --- 1. Pack + compress the NAR into memory ---
    let mut nar_stream = harmonia_nar::NarByteStream::new(store_path.to_owned().into());
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 6)
        .context("failed to create zstd encoder")?;

    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        encoder.write_all(&chunk).context("zstd compression failed")?;
    }

    let compressed = encoder.finish().context("failed to finish zstd encoder")?;
    let nar_size = compressed.len() as u64;
    let nar_hash = format!("sha256:{}", hex::encode(Sha256::digest(&compressed)));

    info!(store_path, nar_size, "uploading NAR to presigned URL");

    // --- 2. HTTP request to the presigned URL ---
    let client = reqwest::Client::new();
    let http_method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("invalid HTTP method: {method}"))?;
    let mut req = client
        .request(http_method, url)
        .header("Content-Type", "application/x-nix-nar")
        .body(compressed);

    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }

    let resp = req.send().await.context("HTTP request to presigned URL failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("presigned upload returned {}: {}", status, body);
    }

    // --- 3. Confirm to the server ---
    conn.send(ClientMessage::NarReady {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        nar_size,
        nar_hash,
    })
    .await?;

    info!(store_path, nar_size, "presigned NAR upload complete");
    Ok(())
}
