/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use futures::StreamExt;
use gradient_wire::constants::BULK_CHUNK_SIZE;
use gradient_wire::messages::ServerMessage;
use gradient_wire::session::frame::{ProtoWriter, send_server_msg};
use tracing::{error, trace, warn};

use crate::{NarSource, NarStore};

pub struct RelayTimeouts {
    pub open: Duration,
    pub chunk_read: Duration,
}

pub struct RelayRequest<'a> {
    pub job_id: &'a str,
    pub store_path: &'a str,
    pub resume_from: u64,
    pub client_token: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Aborted(String),
}

/// Which message a failed transfer sends: [`ServerMessage::NarUnavailable`]
/// before any bytes have streamed, or [`ServerMessage::NarAbort`] mid-stream.
enum FailKind {
    NotFound,
    Unavailable,
    Abort,
}

/// Send the message matching `kind` and return the error every call site
/// returns. Centralizes the abort-and-return idiom `serve_nar_request` used to
/// repeat at every error exit.
async fn fail_transfer(
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    kind: FailKind,
    reason: String,
) -> ServeError {
    match kind {
        FailKind::NotFound | FailKind::Unavailable => {
            let _ = send_server_msg(
                writer,
                &ServerMessage::NarUnavailable {
                    job_id: job_id.to_owned(),
                    store_path: store_path.to_owned(),
                    reason: reason.clone(),
                },
            )
            .await;
        }
        FailKind::Abort => {
            let _ = send_server_msg(
                writer,
                &ServerMessage::NarAbort {
                    job_id: job_id.to_owned(),
                    store_path: store_path.to_owned(),
                    reason: reason.clone(),
                },
            )
            .await;
        }
    }
    match kind {
        FailKind::NotFound => ServeError::NotFound(reason),
        FailKind::Unavailable => ServeError::Unavailable(reason),
        FailKind::Abort => ServeError::Aborted(reason),
    }
}

/// Stream a single requested NAR from `store` to the peer, resuming at
/// `resume_from` when the peer's stream token still matches.
///
/// Hardening notes:
/// - The initial storage open is wrapped in `storage_open_timeout`. A stalled
///   backend (e.g. S3 hung TCP) used to silently consume the dispatch loop's
///   600 s waiter ceiling; now it surfaces as a `NarUnavailable` within the
///   open timeout.
/// - The chunked send path uses [`ProtoWriter`], which bounds per-chunk send
///   waits via the queue + `send_chunk_timeout` configured at split time.
///   A stalled peer is detected as `SendError::Stalled` from `send_server_msg` and
///   triggers a best-effort `NarAbort`.
/// - The body is read from `object_store`'s streaming API - no full file is
///   ever held in memory. Chunks are coalesced/split to `BULK_CHUNK_SIZE`.
/// - Per-chunk read from the storage stream is also bounded so a backend that
///   sends the first byte and then hangs cannot pin the task indefinitely.
pub async fn serve_nar(
    store: &NarStore,
    writer: &ProtoWriter,
    req: RelayRequest<'_>,
    timeouts: RelayTimeouts,
) -> Result<u64, ServeError> {
    let RelayRequest {
        job_id,
        store_path,
        resume_from,
        client_token,
    } = req;
    let storage_open_timeout = timeouts.open;
    let chunk_read_timeout = timeouts.chunk_read;

    let Some(hash) = store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
    else {
        let reason = format!("invalid store path: {store_path}");
        return Err(fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await);
    };

    let open = |offset: u64| async move {
        tokio::time::timeout(storage_open_timeout, store.open(hash, offset)).await
    };

    let mut source = match open(resume_from).await {
        Ok(Ok(Some(source))) => source,
        Ok(Ok(None)) => {
            let reason = format!("NAR not found in cache for {store_path}");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::NotFound, reason).await,
            );
        }
        Ok(Err(e)) => {
            let reason = format!("nar_storage.open({hash}) failed: {e}");
            error!(%store_path, error = %e, "NAR storage read error");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
        Err(_) => {
            let reason = format!(
                "nar_storage.open({hash}) timed out after {}s",
                storage_open_timeout.as_secs()
            );
            warn!(%store_path, "NAR storage open timed out");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
    };

    let size = source.size();

    // The stored `.nar.zst` is immutable per hash, so the pull token is just
    // its size. A worker resuming with a stale token (or claiming more bytes
    // than exist) restarts from 0; the `NarStreamHeader.total_bytes` lets the
    // worker truncate its `.partial` accordingly.
    let server_token = format!("len-{size}");
    let token_mismatch = client_token.is_some_and(|t| t != server_token);
    let mut start = resume_from;
    if resume_from > size || token_mismatch {
        match open(0).await {
            Ok(Ok(Some(s))) => {
                source = s;
                start = 0;
            }
            _ => {
                let reason = format!("failed to reopen {store_path} for fresh transfer");
                return Err(fail_transfer(
                    writer,
                    job_id,
                    store_path,
                    FailKind::Unavailable,
                    reason,
                )
                .await);
            }
        }
    }

    send_server_msg(
        writer,
        &ServerMessage::NarStreamHeader {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            total_bytes: size,
            stream_token: server_token,
        },
    )
    .await
    .ok();

    let mut stream = match source {
        NarSource::Hot(bytes) => {
            let tail = bytes.slice(usize::try_from(start).unwrap_or(0)..);
            futures::stream::once(async move { Ok(tail) }).boxed()
        }
        NarSource::Stream { stream, .. } => stream,
    };

    let mut buf: Vec<u8> = Vec::with_capacity(BULK_CHUNK_SIZE);
    let mut offset: u64 = start;
    let mut total: u64 = 0;
    let mut chunks_sent: u64 = 0;

    loop {
        let next = tokio::time::timeout(chunk_read_timeout, stream.next()).await;
        let item = match next {
            Ok(Some(x)) => x,
            Ok(None) => break,
            Err(_) => {
                let reason = format!(
                    "NAR storage read stalled > {}s mid-transfer",
                    chunk_read_timeout.as_secs()
                );
                warn!(%store_path, "NAR storage read stall");
                return Err(
                    fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await,
                );
            }
        };
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                let reason = format!("NAR storage stream error: {e}");
                error!(%store_path, error = %e, "NAR storage stream error");
                return Err(
                    fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await,
                );
            }
        };

        let mut slice = &bytes[..];
        while !slice.is_empty() {
            let want = BULK_CHUNK_SIZE - buf.len();
            let take = slice.len().min(want);
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == BULK_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(BULK_CHUNK_SIZE));
                let chunk_len = chunk.len() as u64;
                if send_server_msg(
                    writer,
                    &ServerMessage::NarPush {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        data: chunk,
                        offset,
                        is_final: false,
                    },
                )
                .await
                .is_err()
                {
                    let reason = format!("WebSocket send stalled mid-NarPush at offset {offset}");
                    return Err(
                        fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await,
                    );
                }
                offset += chunk_len;
                total += chunk_len;
                chunks_sent += 1;
            }
        }
    }

    let final_len = buf.len() as u64;
    if send_server_msg(
        writer,
        &ServerMessage::NarPush {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            data: buf,
            offset,
            is_final: true,
        },
    )
    .await
    .is_err()
    {
        let reason = format!("WebSocket send stalled on final NarPush at offset {offset}");
        return Err(fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await);
    }
    total += final_len;
    chunks_sent += 1;

    trace!(%store_path, bytes = total, chunks = chunks_sent, "NarRequest served (streaming)");
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gradient_util::shutdown::Shutdown;
    use gradient_wire::constants::BULK_CHUNK_SIZE;
    use gradient_wire::messages::ServerMessage;
    use gradient_wire::testing::loopback;
    use tempfile::TempDir;

    use super::*;

    const HASH: &str = "0c5bbq5r7gbpcbym1ilmd0r3mcjq7dfy";
    const PATH: &str = "/nix/store/0c5bbq5r7gbpcbym1ilmd0r3mcjq7dfy-hello";

    fn timeouts() -> RelayTimeouts {
        RelayTimeouts {
            open: Duration::from_secs(5),
            chunk_read: Duration::from_secs(5),
        }
    }

    fn empty_store() -> (TempDir, NarStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = NarStore::local(dir.path().to_str().expect("utf8")).expect("local store");
        (dir, store)
    }

    async fn stored(len: usize) -> (TempDir, NarStore) {
        let (dir, store) = empty_store();
        store.put(HASH, vec![7; len]).await.expect("put");
        (dir, store)
    }

    fn is_terminal(msg: &ServerMessage) -> bool {
        matches!(
            msg,
            ServerMessage::NarPush { is_final: true, .. }
                | ServerMessage::NarUnavailable { .. }
                | ServerMessage::NarAbort { .. }
        )
    }

    async fn serve(
        store: &NarStore,
        store_path: &str,
        resume_from: u64,
        client_token: Option<&str>,
    ) -> (Result<u64, ServeError>, Vec<ServerMessage>) {
        let (authority, mut peer) = loopback().await;
        let shutdown = Shutdown::new();
        let (_reader, writer) = authority.split(Duration::from_secs(5), &shutdown);
        let req = RelayRequest {
            job_id: "build:1",
            store_path,
            resume_from,
            client_token,
        };
        let result = serve_nar(store, &writer, req, timeouts()).await;
        let mut frames = Vec::new();
        while let Some(msg) = peer.recv_server_msg().await {
            let done = is_terminal(&msg);
            frames.push(msg);
            if done {
                break;
            }
        }

        (result, frames)
    }

    fn first_push_offset(frames: &[ServerMessage]) -> u64 {
        frames
            .iter()
            .find_map(|m| match m {
                ServerMessage::NarPush { offset, .. } => Some(*offset),
                _ => None,
            })
            .expect("a NarPush frame")
    }

    #[tokio::test]
    async fn streams_a_header_then_bulk_chunks_then_a_final_frame() {
        let len = BULK_CHUNK_SIZE + BULK_CHUNK_SIZE / 2;
        let (_dir, store) = stored(len).await;
        let (result, frames) = serve(&store, PATH, 0, None).await;
        assert_eq!(result.expect("served"), len as u64);
        let ServerMessage::NarStreamHeader {
            total_bytes,
            stream_token,
            ..
        } = &frames[0]
        else {
            panic!("header first, got {frames:?}");
        };
        assert_eq!(*total_bytes, len as u64);
        assert_eq!(stream_token.as_str(), format!("len-{len}"));
        assert!(matches!(
            &frames[1],
            ServerMessage::NarPush { offset: 0, is_final: false, data, .. } if data.len() == BULK_CHUNK_SIZE
        ));
        assert!(matches!(
            &frames[2],
            ServerMessage::NarPush { is_final: true, offset, .. } if *offset == BULK_CHUNK_SIZE as u64
        ));
    }

    #[tokio::test]
    async fn a_matching_token_resumes_from_the_offset() {
        let (_dir, store) = stored(1000).await;
        let (_, frames) = serve(&store, PATH, 100, Some("len-1000")).await;
        assert_eq!(first_push_offset(&frames), 100);
    }

    #[tokio::test]
    async fn a_stale_token_or_an_offset_past_the_end_restarts_from_zero() {
        let (_dir, store) = stored(1000).await;
        let (_, stale) = serve(&store, PATH, 100, Some("len-1")).await;
        assert_eq!(first_push_offset(&stale), 0);

        let (_, past_end) = serve(&store, PATH, 1001, None).await;
        assert_eq!(first_push_offset(&past_end), 0);
    }

    #[tokio::test]
    async fn a_missing_object_sends_nar_unavailable_and_reports_not_found() {
        let (_dir, store) = empty_store();
        let (result, frames) = serve(&store, PATH, 0, None).await;
        assert!(matches!(result, Err(ServeError::NotFound(_))));
        assert!(matches!(
            frames.as_slice(),
            [ServerMessage::NarUnavailable { .. }]
        ));
    }

    #[tokio::test]
    async fn an_invalid_store_path_is_unavailable() {
        let (_dir, store) = stored(10).await;
        let (result, frames) = serve(&store, "not-a-store-path", 0, None).await;
        assert!(matches!(result, Err(ServeError::Unavailable(_))));
        assert!(matches!(
            frames.as_slice(),
            [ServerMessage::NarUnavailable { .. }]
        ));
    }
}
