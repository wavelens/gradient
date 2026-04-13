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

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::prelude::MockProtoServer;

    /// Create a temporary directory with a single file and return its path.
    fn make_temp_store_path() -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("gradient-nar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello"), b"gradient nar test data").unwrap();
        dir
    }

    #[tokio::test]
    async fn push_direct_sends_chunks_and_final() {
        let store_path = make_temp_store_path();
        let store_path_str = store_path.to_str().unwrap().to_owned();

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let mut chunks: Vec<(Vec<u8>, u64, bool)> = Vec::new();

            loop {
                let msg = sc.recv().await.unwrap();
                if let ClientMessage::NarPush { data, offset, is_final, .. } = msg {
                    let done = is_final;
                    chunks.push((data, offset, is_final));
                    if done { break; }
                } else {
                    panic!("expected NarPush, got {msg:?}");
                }
            }

            // At least one non-empty data chunk + one final empty chunk.
            assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());

            let last = chunks.last().unwrap();
            assert!(last.2, "last chunk must be final");
            assert!(last.0.is_empty(), "final chunk data must be empty");

            // Offsets must be monotonically non-decreasing.
            let offsets: Vec<u64> = chunks.iter().map(|(_, o, _)| *o).collect();
            for w in offsets.windows(2) {
                assert!(w[1] >= w[0], "offsets not monotonic: {offsets:?}");
            }
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        push_direct("job-123", &store_path_str, &mut conn).await.unwrap();

        server_task.await.unwrap();
        let _ = std::fs::remove_dir_all(&store_path);
    }

    #[tokio::test]
    async fn push_direct_data_is_valid_zstd() {
        let store_path = make_temp_store_path();
        let store_path_str = store_path.to_str().unwrap().to_owned();

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let mut all_data: Vec<u8> = Vec::new();

            loop {
                let msg = sc.recv().await.unwrap();
                if let ClientMessage::NarPush { data, is_final, .. } = msg {
                    all_data.extend_from_slice(&data);
                    if is_final { break; }
                }
            }

            // The concatenated data (minus the empty final chunk) must be valid zstd.
            let decoded = zstd::decode_all(std::io::Cursor::new(&all_data))
                .expect("zstd decompression failed");
            assert!(!decoded.is_empty(), "decompressed NAR should not be empty");
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        push_direct("job-123", &store_path_str, &mut conn).await.unwrap();

        server_task.await.unwrap();
        let _ = std::fs::remove_dir_all(&store_path);
    }

    /// Minimal HTTP server that accepts one PUT and replies 200.
    async fn one_shot_http_server() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/upload");

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read until we see the end of headers (blank line).
            let mut buf = vec![0u8; 65536];
            let mut total = 0;
            loop {
                let n = stream.read(&mut buf[total..]).await.unwrap();
                if n == 0 { break; }
                total += n;
                // Look for \r\n\r\n (end of HTTP headers).
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // Reply 200 OK.
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await.unwrap();
            // Return the received bytes (headers + body prefix).
            buf[..total].to_vec()
        });

        (url, handle)
    }

    #[tokio::test]
    async fn upload_presigned_sends_nar_ready() {
        let store_path = make_temp_store_path();
        let store_path_str = store_path.to_str().unwrap().to_owned();

        let (http_url, _http_task) = one_shot_http_server().await;

        let server = MockProtoServer::bind().await;
        let url = server.url().to_owned();

        let server_task = tokio::spawn(async move {
            let mut sc = server.accept().await;
            let msg = sc.recv().await.unwrap();
            if let ClientMessage::NarReady { job_id, store_path: sp, nar_size, nar_hash } = msg {
                assert_eq!(job_id, "job-xyz");
                assert!(!sp.is_empty());
                assert!(nar_size > 0, "nar_size should be nonzero");
                assert!(nar_hash.starts_with("sha256:"), "nar_hash should be sha256: SRI, got {nar_hash}");
            } else {
                panic!("expected NarReady, got {msg:?}");
            }
        });

        let mut conn = crate::connection::ProtoConnection::open(&url).await.unwrap();
        upload_presigned("job-xyz", &store_path_str, &http_url, "PUT", &[], &mut conn)
            .await
            .unwrap();

        server_task.await.unwrap();
        let _ = std::fs::remove_dir_all(&store_path);
    }
}
