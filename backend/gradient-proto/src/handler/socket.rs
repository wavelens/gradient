/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! State-coupled helpers built on top of [`crate::session::frame`].
//!
//! Pure framing (the [`ProtoSocket`]/[`ProtoReader`]/[`ProtoWriter`]
//! abstractions and the underlying constants) moved to
//! [`crate::session::frame`]. What remains here are the helpers that need
//! access to [`ServerState`] / [`Scheduler`]: NAR serving from
//! `nar_storage`, credential delivery, and the cached-path self-heal.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gradient_types::ids::OrganizationId;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tracing::{debug, error, warn};

use crate::messages::ServerMessage;
use gradient_scheduler::Scheduler;

pub use crate::session::frame::{
    HANDSHAKE_TIMEOUT, JOB_OFFER_CHUNK_SIZE, NAR_PUSH_CHUNK_SIZE, ProtoSocket, ProtoWriter,
    recv_client_msg, send_error, send_server_msg,
};

/// Push any pending job candidates to the worker (delta).
pub(super) async fn push_pending_candidates(
    writer: &ProtoWriter,
    scheduler: &Scheduler,
    peer_id: &str,
) {
    let candidates = scheduler.get_new_job_candidates(peer_id).await;
    if candidates.is_empty() {
        return;
    }
    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta) after message processing");
    for chunk in candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
        let _ = send_server_msg(
            writer,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await;
    }
}

// ── NAR streaming ─────────────────────────────────────────────────────────────

/// Stream a single requested NAR from `nar_storage` to the worker.
///
/// Hardening notes:
/// - The initial storage open is wrapped in `storage_open_timeout`. A stalled
///   backend (e.g. S3 hung TCP) used to silently consume the dispatch loop's
///   600 s waiter ceiling; now it surfaces as a `NarUnavailable` within the
///   open timeout.
/// - The chunked send path uses [`ProtoWriter`], which bounds per-chunk send
///   waits via the queue + `send_chunk_timeout` configured at split time.
///   A stalled peer is detected as `Err(())` from `send_server_msg` and
///   triggers a best-effort `NarAbort`.
/// - The body is read from `object_store`'s streaming API - no full file is
///   ever held in memory. Chunks are coalesced/split to `NAR_PUSH_CHUNK_SIZE`.
/// - Per-chunk read from the storage stream is also bounded so a backend that
///   sends the first byte and then hangs cannot pin the task indefinitely.
pub(super) async fn serve_nar_request(
    state: &Arc<ServerState>,
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
) -> anyhow::Result<()> {
    let proto_cfg = &state.config.proto;
    let storage_open_timeout = Duration::from_secs(proto_cfg.nar_storage_open_timeout_secs);
    let chunk_read_timeout = Duration::from_secs(proto_cfg.nar_send_chunk_timeout_secs);

    let Some(hash) = store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
    else {
        let reason = format!("invalid store path: {store_path}");
        send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
        return Err(anyhow::anyhow!(reason));
    };

    let opened =
        tokio::time::timeout(storage_open_timeout, state.nar_storage.get_stream(hash)).await;
    let mut stream = match opened {
        Ok(Ok(Some((_size, s)))) => s,
        Ok(Ok(None)) => {
            invalidate_cached_path(state, hash, store_path).await;
            let reason = format!("NAR not found in cache for {store_path}");
            send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
            return Err(anyhow::anyhow!(reason));
        }
        Ok(Err(e)) => {
            let reason = format!("nar_storage.get_stream({hash}) failed: {e}");
            error!(%store_path, error = %e, "NAR storage read error");
            send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
            return Err(anyhow::anyhow!(reason));
        }
        Err(_) => {
            let reason = format!(
                "nar_storage.get_stream({hash}) timed out after {}s",
                storage_open_timeout.as_secs()
            );
            warn!(%store_path, "NAR storage open timed out");
            send_nar_unavailable(writer, job_id, store_path, reason.clone()).await;
            return Err(anyhow::anyhow!(reason));
        }
    };

    let mut buf: Vec<u8> = Vec::with_capacity(NAR_PUSH_CHUNK_SIZE);
    let mut offset: u64 = 0;
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
                let _ = send_server_msg(
                    writer,
                    &ServerMessage::NarAbort {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        reason: reason.clone(),
                    },
                )
                .await;
                return Err(anyhow::anyhow!(reason));
            }
        };
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                let reason = format!("NAR storage stream error: {e}");
                error!(%store_path, error = %e, "NAR storage stream error");
                let _ = send_server_msg(
                    writer,
                    &ServerMessage::NarAbort {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        reason: reason.clone(),
                    },
                )
                .await;
                return Err(anyhow::anyhow!(reason));
            }
        };

        let mut slice = &bytes[..];
        while !slice.is_empty() {
            let want = NAR_PUSH_CHUNK_SIZE - buf.len();
            let take = slice.len().min(want);
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == NAR_PUSH_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(NAR_PUSH_CHUNK_SIZE));
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
                    let _ = send_server_msg(
                        writer,
                        &ServerMessage::NarAbort {
                            job_id: job_id.to_owned(),
                            store_path: store_path.to_owned(),
                            reason: reason.clone(),
                        },
                    )
                    .await;
                    return Err(anyhow::anyhow!(reason));
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
        let _ = send_server_msg(
            writer,
            &ServerMessage::NarAbort {
                job_id: job_id.to_owned(),
                store_path: store_path.to_owned(),
                reason: reason.clone(),
            },
        )
        .await;
        return Err(anyhow::anyhow!(reason));
    }
    total += final_len;
    chunks_sent += 1;

    debug!(%store_path, bytes = total, chunks = chunks_sent, "NarRequest served (streaming)");
    Ok(())
}

/// Demote a `cached_path` row whose NAR is no longer in `nar_storage`.
///
/// Sets `file_hash` / `nar_hash` / `file_size` / `nar_size` to NULL so
/// `Model::is_fully_cached()` flips to `false` and the next `CacheQuery`
/// stops claiming the path is available - letting the next build either
/// rebuild from source or pick the path up from a configured upstream.
async fn invalidate_cached_path(state: &Arc<ServerState>, hash: &str, store_path: &str) {
    let row = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.worker_db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            warn!(%hash, %store_path, error = %e, "self-heal: cached_path lookup failed");
            return;
        }
    };
    let cached_path_id = row.id;
    let mut active: ACachedPath = row.into();
    active.file_hash = Set(None);
    active.nar_hash = Set(None);
    active.file_size = Set(None);
    active.nar_size = Set(None);
    if let Err(e) = active.update(&state.worker_db).await {
        warn!(%hash, %store_path, error = %e, "self-heal: failed to demote cached_path row");
        return;
    }

    let outputs = match EDerivationOutput::find()
        .filter(CDerivationOutput::CachedPath.eq(cached_path_id))
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(%hash, %store_path, error = %e, "self-heal: derivation_output lookup failed");
            return;
        }
    };
    for o in outputs {
        let mut active: ADerivationOutput = o.into();
        active.is_cached = Set(false);
        if let Err(e) = active.update(&state.worker_db).await {
            warn!(%hash, %store_path, error = %e, "self-heal: failed to clear derivation_output.is_cached");
        }
    }

    warn!(
        %hash,
        %store_path,
        "self-heal: NAR missing from storage; cached_path demoted so the path will be rebuilt"
    );
}

async fn send_nar_unavailable(
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    reason: String,
) {
    let _ = send_server_msg(
        writer,
        &ServerMessage::NarUnavailable {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            reason,
        },
    )
    .await;
}

// ── Credential delivery ───────────────────────────────────────────────────────

pub(super) async fn send_credentials_for_job(
    writer: &ProtoWriter,
    state: &ServerState,
    scheduler: &gradient_scheduler::Scheduler,
    worker_id: &str,
    job: &gradient_types::proto::Job,
    org_id: OrganizationId,
) {
    use gradient_types::proto::{FlakeTask, Job};

    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let worker_can_fetch = caps.as_ref().map(|c| c.fetch).unwrap_or(false);

    match job {
        Job::Flake(flake_job) => {
            if worker_can_fetch && flake_job.tasks.contains(&FlakeTask::FetchFlake) {
                send_ssh_key_credential(writer, state, org_id).await;
            }
        }
        Job::Build(_) => {}
    }
}

async fn send_ssh_key_credential(
    writer: &ProtoWriter,
    state: &ServerState,
    org_id: OrganizationId,
) {
    use gradient_types::proto::CredentialKind;

    match EOrganization::find_by_id(org_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(org)) => {
            match gradient_sources::ssh_key::decrypt_ssh_private_key(
                &state.config.secrets.crypt_secret_file,
                org,
                &state.config.server.serve_url,
            ) {
                Ok((private_key, _public_key)) => {
                    let _ = send_server_msg(
                        writer,
                        &ServerMessage::Credential {
                            kind: CredentialKind::SshKey,
                            data: private_key.into_bytes(),
                        },
                    )
                    .await;
                    debug!(%org_id, "SSH key credential sent");
                }
                Err(e) => {
                    debug!(%org_id, error = %e, "no SSH key for org (may be HTTPS repo)");
                }
            }
        }
        Ok(None) => warn!(%org_id, "org not found for SSH key lookup"),
        Err(e) => warn!(%org_id, error = %e, "failed to fetch org for SSH key"),
    }
}

#[cfg(test)]
mod serve_nar_tests {
    use super::*;
    use crate::messages::decode_server_message;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use gradient_test_support::state::test_state;
    use tokio::sync::mpsc;

    /// Spy writer: records every message the server attempted to send so the
    /// test can assert exactly which protocol frames were emitted (NarPush,
    /// NarUnavailable, NarAbort, …).
    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        (
            ProtoWriter {
                tx,
                send_chunk_timeout: timeout,
            },
            rx,
        )
    }

    fn decode(bytes: &[u8]) -> ServerMessage {
        decode_server_message(bytes).expect("decode ServerMessage")
    }

    fn variant_of(msg: &ServerMessage) -> &'static str {
        match msg {
            ServerMessage::NarPush { .. } => "NarPush",
            ServerMessage::NarUnavailable { .. } => "NarUnavailable",
            ServerMessage::NarAbort { .. } => "NarAbort",
            _ => "other",
        }
    }

    /// Streamed payload arrives as one or more `NarPush` frames whose
    /// concatenated `data` matches the original bytes, with the final frame
    /// flagged `is_final=true`.
    #[tokio::test]
    async fn serve_streams_full_payload_in_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let mut payload = Vec::with_capacity(9 * 1024 * 1024);
        for i in 0..(9 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state.nar_storage.put(hash, payload.clone()).await.unwrap();

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(&state, &writer, "job-1", &store_path)
            .await
            .expect("serve must succeed");

        let mut assembled = Vec::with_capacity(payload.len());
        let mut frames = 0u32;
        let mut saw_final = false;
        while let Ok(bytes) = rx.try_recv() {
            let msg = decode(&bytes);
            assert_eq!(variant_of(&msg), "NarPush", "only NarPush frames expected");
            if let ServerMessage::NarPush { data, is_final, .. } = msg {
                assembled.extend_from_slice(&data);
                if is_final {
                    saw_final = true;
                }
            }
            frames += 1;
        }
        assert!(
            frames >= 3,
            "9 MiB / 4 MiB chunks → at least 3 frames, got {frames}"
        );
        assert!(saw_final, "the last frame must be is_final=true");
        assert_eq!(
            assembled, payload,
            "concatenated NarPush data must equal source"
        );
    }

    /// Missing object → `NarUnavailable` (not `NarAbort`, no NarPush) and an
    /// `Err` from `serve_nar_request`.
    #[tokio::test]
    async fn serve_emits_nar_unavailable_when_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &state,
            &writer,
            "job-1",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-missing",
        )
        .await;
        assert!(res.is_err(), "missing path must surface as Err");

        let bytes = rx.try_recv().expect("expect one frame");
        let msg = decode(&bytes);
        assert_eq!(variant_of(&msg), "NarUnavailable");
        assert!(
            rx.try_recv().is_err(),
            "no further frames after NarUnavailable"
        );
    }
}
