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

use std::collections::BTreeSet;
use std::io::Write as _;

use anyhow::{Context, Result};
use base64::Engine as _;
use futures::StreamExt;
use harmonia_store_core::signature::{SecretKey, fingerprint_path};
use harmonia_store_core::store_path::{StoreDir, StorePath};
use harmonia_store_remote::DaemonStore as _;
use harmonia_utils_hash::fmt::CommonHash as _;
use proto::messages::ClientMessage;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::connection::ProtoWriter;
use crate::nix::store::{LocalNixStore, strip_store_prefix};

/// Chunk size for direct NAR streaming (64 KiB).
const NAR_CHUNK_SIZE: usize = 64 * 1024;

/// Path metadata gathered from the local nix-daemon for a built store path.
struct PathMeta {
    /// References in hash-name format (without `/nix/store/` prefix).
    references: Vec<String>,
    /// Optional base64 cache signature (after the `name:` prefix).
    signature: Option<String>,
}

/// Query the local nix-daemon for `store_path`'s references and optionally
/// compute a cache signature.
///
/// Returns `None` (and logs a warning) if the path is not found in the store
/// or if any step fails — NAR upload continues without metadata in that case.
async fn gather_path_meta(
    store: &LocalNixStore,
    store_path: &str,
    signing_key_str: Option<&str>,
) -> Option<PathMeta> {
    let base = strip_store_prefix(store_path);
    let sp = match StorePath::from_base_path(base) {
        Ok(sp) => sp,
        Err(e) => {
            warn!(store_path, error = %e, "gather_path_meta: invalid store path");
            return None;
        }
    };

    let mut guard = match store.pool().acquire().await {
        Ok(g) => g,
        Err(e) => {
            warn!(store_path, error = %e, "gather_path_meta: could not acquire store connection");
            return None;
        }
    };

    let path_info = match guard.client().query_path_info(&sp).await {
        Ok(Some(pi)) => pi,
        Ok(None) => {
            warn!(store_path, "gather_path_meta: path not found in local store");
            return None;
        }
        Err(e) => {
            warn!(store_path, error = %e, "gather_path_meta: query_path_info failed");
            return None;
        }
    };

    let references_raw: Vec<StorePath> = path_info.references.iter()
        .filter_map(|r: &StorePath| {
            StorePath::from_base_path(strip_store_prefix(&r.to_string())).ok()
        })
        .collect();

    let references: Vec<String> = path_info
        .references
        .iter()
        .map(|r: &StorePath| {
            let s = r.to_string();
            s.strip_prefix("/nix/store/").unwrap_or(&s).to_owned()
        })
        .collect();

    let nar_hash_sri = path_info.nar_hash.sri().to_string();
    let nar_size = path_info.nar_size;

    let signature = signing_key_str.and_then(|key_str| {
        compute_signature(&sp, &nar_hash_sri, nar_size, &references_raw, key_str)
            .map_err(|e| warn!(store_path, error = %e, "failed to compute cache signature"))
            .ok()
    });

    Some(PathMeta {
        references,
        signature,
    })
}

fn compute_signature(
    sp: &StorePath,
    nar_hash_sri: &str,
    nar_size: u64,
    references_raw: &[StorePath],
    signing_key_str: &str,
) -> Result<String> {
    let secret_key: SecretKey = signing_key_str
        .parse()
        .map_err(|e| anyhow::anyhow!("failed to parse signing key: {}", e))?;

    let nar_hash_nix = sri_to_nix_hash(nar_hash_sri).context("convert NAR hash")?;

    let store_dir = StoreDir::default();
    let refs_set: BTreeSet<StorePath> = references_raw.iter().cloned().collect();

    let fingerprint = fingerprint_path(
        &store_dir,
        sp,
        nar_hash_nix.as_bytes(),
        nar_size,
        &refs_set,
    )
    .context("compute fingerprint")?;

    let sig = secret_key.sign(&fingerprint);
    let sig_str = sig.to_string();
    // Strip the `name:` prefix — caller reconstructs it with the cache name.
    let b64 = sig_str
        .find(':')
        .map(|i| sig_str[i + 1..].to_string())
        .unwrap_or(sig_str);

    Ok(b64)
}

fn sri_to_nix_hash(sri: &str) -> Result<String> {
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

/// Compress `store_path` into a zstd-compressed NAR and push it to the server
/// in [`NAR_CHUNK_SIZE`]-byte chunks via [`ClientMessage::NarPush`].
///
/// This is the "direct" transfer mode — no S3 involved.
///
/// When `store` and `signing_key_str` are provided the function also queries
/// the local store for references and computes a cache signature; both are
/// included in the final [`ClientMessage::NarUploaded`] so the server can
/// populate `cached_path` / `cached_path_signature` rows.
pub async fn push_direct(
    job_id: &str,
    store_path: &str,
    writer: &ProtoWriter,
    store: Option<&LocalNixStore>,
    signing_key_str: Option<&str>,
) -> Result<()> {
    debug!(store_path, "NAR direct push");

    let mut nar_stream = harmonia_nar::NarByteStream::new(store_path.to_owned().into());
    let mut encoder = zstd::stream::Encoder::new(Vec::with_capacity(NAR_CHUNK_SIZE * 2), 6)
        .context("failed to create zstd encoder")?;
    let mut file_hasher = Sha256::new();
    let mut nar_hasher = Sha256::new();
    let mut offset: u64 = 0;
    let mut nar_size: u64 = 0;

    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        nar_hasher.update(&chunk);
        nar_size += chunk.len() as u64;
        encoder
            .write_all(&chunk)
            .context("zstd compression failed")?;

        let buf = encoder.get_mut();
        while buf.len() >= NAR_CHUNK_SIZE {
            let part: Vec<u8> = buf.drain(..NAR_CHUNK_SIZE).collect();
            let len = part.len() as u64;
            file_hasher.update(&part);
            writer.send(ClientMessage::NarPush {
                job_id: job_id.to_owned(),
                store_path: store_path.to_owned(),
                data: part,
                offset,
                is_final: false,
            })?;
            offset += len;
        }
    }

    let remaining = encoder.finish().context("failed to finish zstd encoder")?;
    if !remaining.is_empty() {
        let len = remaining.len() as u64;
        file_hasher.update(&remaining);
        writer.send(ClientMessage::NarPush {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            data: remaining,
            offset,
            is_final: false,
        })?;
        offset += len;
    }

    // Empty final chunk signals end-of-path.
    writer.send(ClientMessage::NarPush {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        data: vec![],
        offset,
        is_final: true,
    })?;

    let file_hash = format!("sha256:{}", hex::encode(file_hasher.finalize()));
    let nar_hash = format!("sha256:{}", hex::encode(nar_hasher.finalize()));

    let (references, signature) = if let Some(s) = store {
        let meta = gather_path_meta(s, store_path, signing_key_str).await;
        (
            meta.as_ref().map(|m| m.references.clone()).unwrap_or_default(),
            meta.and_then(|m| m.signature),
        )
    } else {
        (vec![], None)
    };

    // Report metadata so the server can update cache records.
    writer.send(ClientMessage::NarUploaded {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        file_hash: file_hash.clone(),
        file_size: offset,
        nar_size,
        nar_hash,
        references,
        signature,
    })?;

    info!(
        store_path,
        compressed_bytes = offset,
        %file_hash,
        "direct NAR push complete"
    );
    Ok(())
}

/// Upload `store_path` as a zstd-compressed NAR to the presigned `url`, then
/// send [`ClientMessage::NarReady`] with the compressed size and SHA-256 hash.
///
/// `method` is the HTTP method the server expects (usually `"PUT"`).
/// `headers` are additional HTTP headers to include (e.g. `x-amz-*` for S3).
///
/// When `store` and `signing_key_str` are provided the function also queries
/// the local store for references and computes a cache signature; both are
/// included in the final [`ClientMessage::NarUploaded`].
pub async fn upload_presigned(
    job_id: &str,
    store_path: &str,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    writer: &ProtoWriter,
    store: Option<&LocalNixStore>,
    signing_key_str: Option<&str>,
) -> Result<()> {
    debug!(store_path, method, "presigned NAR upload");

    // --- 1. Pack + compress the NAR into memory ---
    let mut nar_stream = harmonia_nar::NarByteStream::new(store_path.to_owned().into());
    let mut encoder =
        zstd::stream::Encoder::new(Vec::new(), 6).context("failed to create zstd encoder")?;
    let mut nar_hasher = Sha256::new();
    let mut nar_size: u64 = 0;

    while let Some(chunk_result) = nar_stream.next().await {
        let chunk = chunk_result.context("NAR stream error")?;
        nar_hasher.update(&chunk);
        nar_size += chunk.len() as u64;
        encoder
            .write_all(&chunk)
            .context("zstd compression failed")?;
    }

    let compressed = encoder.finish().context("failed to finish zstd encoder")?;
    let file_size = compressed.len() as u64;
    let file_hash = format!("sha256:{}", hex::encode(Sha256::digest(&compressed)));
    let nar_hash = format!("sha256:{}", hex::encode(nar_hasher.finalize()));

    info!(
        store_path,
        file_size, nar_size, "uploading NAR to presigned URL"
    );

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

    let resp = req
        .send()
        .await
        .context("HTTP request to presigned URL failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("presigned upload returned {}: {}", status, body);
    }

    // --- 3. Gather path metadata (references + signature) ---
    let (references, signature) = if let Some(s) = store {
        let meta = gather_path_meta(s, store_path, signing_key_str).await;
        (
            meta.as_ref().map(|m| m.references.clone()).unwrap_or_default(),
            meta.and_then(|m| m.signature),
        )
    } else {
        (vec![], None)
    };

    // --- 4. Confirm to the server ---
    writer.send(ClientMessage::NarReady {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        nar_size: file_size,
        nar_hash: file_hash.clone(),
    })?;

    writer.send(ClientMessage::NarUploaded {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        file_hash,
        file_size,
        nar_size,
        nar_hash,
        references,
        signature,
    })?;

    info!(
        store_path,
        file_size, nar_size, "presigned NAR upload complete"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::prelude::MockProtoServer;

    /// Create a temporary directory with a single file and return its path.
    ///
    /// Each call produces a unique directory (UUID-based) so parallel tests
    /// don't interfere with each other's cleanup.
    fn make_temp_store_path() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gradient-nar-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
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
                if let ClientMessage::NarPush {
                    data,
                    offset,
                    is_final,
                    ..
                } = msg
                {
                    let done = is_final;
                    chunks.push((data, offset, is_final));
                    if done {
                        break;
                    }
                } else {
                    panic!("expected NarPush, got {msg:?}");
                }
            }

            // At least one non-empty data chunk + one final empty chunk.
            assert!(
                chunks.len() >= 2,
                "expected at least 2 chunks, got {}",
                chunks.len()
            );

            let last = chunks.last().unwrap();
            assert!(last.2, "last chunk must be final");
            assert!(last.0.is_empty(), "final chunk data must be empty");

            // Offsets must be monotonically non-decreasing.
            let offsets: Vec<u64> = chunks.iter().map(|(_, o, _)| *o).collect();
            for w in offsets.windows(2) {
                assert!(w[1] >= w[0], "offsets not monotonic: {offsets:?}");
            }
        });

        let conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, _reader) = conn.split();
        push_direct("job-123", &store_path_str, &writer, None, None)
            .await
            .unwrap();

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
                    if is_final {
                        break;
                    }
                }
            }

            // The concatenated data (minus the empty final chunk) must be valid zstd.
            let decoded = zstd::decode_all(std::io::Cursor::new(&all_data))
                .expect("zstd decompression failed");
            assert!(!decoded.is_empty(), "decompressed NAR should not be empty");
        });

        let conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, _reader) = conn.split();
        push_direct("job-123", &store_path_str, &writer, None, None)
            .await
            .unwrap();

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
                if n == 0 {
                    break;
                }
                total += n;
                // Look for \r\n\r\n (end of HTTP headers).
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // Reply 200 OK.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
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
            if let ClientMessage::NarReady {
                job_id,
                store_path: sp,
                nar_size,
                nar_hash,
            } = msg
            {
                assert_eq!(job_id, "job-xyz");
                assert!(!sp.is_empty());
                assert!(nar_size > 0, "nar_size should be nonzero");
                assert!(
                    nar_hash.starts_with("sha256:"),
                    "nar_hash should be sha256: SRI, got {nar_hash}"
                );
            } else {
                panic!("expected NarReady, got {msg:?}");
            }
        });

        let conn = crate::connection::ProtoConnection::open(&url)
            .await
            .unwrap();
        let (writer, _reader) = conn.split();
        upload_presigned("job-xyz", &store_path_str, &http_url, "PUT", &[], &writer, None, None)
            .await
            .unwrap();

        server_task.await.unwrap();
        let _ = std::fs::remove_dir_all(&store_path);
    }
}
