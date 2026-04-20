/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Low-level WebSocket I/O, credential delivery, and NAR streaming.

use std::sync::Arc;

use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use gradient_core::types::*;
use rkyv::rancor::Error as RkyvError;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, tungstenite::Message as TungsteniteMessage,
};
use tracing::{debug, trace, warn};
use uuid::Uuid;

use crate::messages::{ClientMessage, ServerMessage};
use scheduler::Scheduler;

// ── Constants ─────────────────────────────────────────────────────────────────

pub(super) const JOB_OFFER_CHUNK_SIZE: usize = 1_000;
pub(super) const NAR_PUSH_CHUNK_SIZE: usize = 64 * 1024;

// ── Socket abstraction ────────────────────────────────────────────────────────

/// Wraps both axum and raw tungstenite WebSocket streams so `handle_socket` can
/// drive connections regardless of who initiated the transport.
pub(crate) enum ProtoSocket {
    /// Inbound: worker connected to the server's `/proto` endpoint.
    Axum(WebSocket),
    /// Outbound: server connected to a worker's listener.
    Tungstenite(WebSocketStream<MaybeTlsStream<TcpStream>>),
}

impl ProtoSocket {
    pub(super) async fn recv_bytes(&mut self) -> Option<Result<Vec<u8>, ()>> {
        match self {
            Self::Axum(ws) => loop {
                match ws.recv().await? {
                    Ok(AxumMessage::Binary(bytes)) => return Some(Ok(bytes.to_vec())),
                    Ok(AxumMessage::Close(_)) => return None,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!(error = %e, "WebSocket recv error");
                        return None;
                    }
                }
            },
            Self::Tungstenite(ws) => loop {
                match ws.next().await? {
                    Ok(TungsteniteMessage::Binary(bytes)) => return Some(Ok(bytes.to_vec())),
                    Ok(TungsteniteMessage::Close(_)) => return None,
                    Ok(TungsteniteMessage::Ping(_) | TungsteniteMessage::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        debug!(error = %e, "WebSocket recv error");
                        return None;
                    }
                }
            },
        }
    }

    pub(super) async fn send_bytes(&mut self, bytes: Vec<u8>) -> Result<(), ()> {
        match self {
            Self::Axum(ws) => ws
                .send(AxumMessage::Binary(bytes.into()))
                .await
                .map_err(|e| debug!(error = %e, "WebSocket send error")),
            Self::Tungstenite(ws) => ws
                .send(TungsteniteMessage::Binary(bytes.into()))
                .await
                .map_err(|e| debug!(error = %e, "WebSocket send error")),
        }
    }
}

// ── Message framing ───────────────────────────────────────────────────────────

pub(super) async fn recv_client_msg(socket: &mut ProtoSocket) -> Option<ClientMessage> {
    let bytes = match socket.recv_bytes().await? {
        Ok(b) => b,
        Err(()) => return None,
    };
    match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
        Ok(msg) => {
            trace!(?msg, bytes = bytes.len(), "recv ClientMessage");
            Some(msg)
        }
        Err(e) => {
            warn!(error = %e, "failed to deserialize client message");
            send_error(socket, 400, "malformed message".into()).await;
            None
        }
    }
}

pub(super) async fn send_server_msg(
    socket: &mut ProtoSocket,
    msg: &ServerMessage,
) -> Result<(), ()> {
    let bytes = rkyv::to_bytes::<RkyvError>(msg).map_err(|e| {
        warn!(error = %e, "failed to serialize server message");
    })?;
    trace!(?msg, bytes = bytes.len(), "send ServerMessage");
    socket.send_bytes(bytes.to_vec()).await
}

pub(super) async fn send_error(socket: &mut ProtoSocket, code: u16, message: String) {
    let _ = send_server_msg(socket, &ServerMessage::Error { code, message }).await;
}

pub(super) async fn send_reject(socket: &mut ProtoSocket, code: u16, reason: String) {
    let _ = send_server_msg(socket, &ServerMessage::Reject { code, reason }).await;
}

/// Push any pending job candidates to the worker (delta).
///
/// Called after processing messages that unlock new jobs (EvalResult,
/// JobCompleted, JobFailed). Can't rely on `job_notify` here since we're
/// not inside the `select!` loop when processing those messages.
pub(super) async fn push_pending_candidates(
    socket: &mut ProtoSocket,
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
            socket,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await;
    }
}

// ── NAR streaming ─────────────────────────────────────────────────────────────

pub(super) async fn serve_nar_request(
    state: &Arc<ServerState>,
    socket: &mut ProtoSocket,
    job_id: &str,
    store_path: &str,
) -> anyhow::Result<()> {
    let hash = store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
        .ok_or_else(|| anyhow::anyhow!("invalid store path: {store_path}"))?;

    let bytes = state
        .nar_storage
        .get(hash)
        .await
        .map_err(|e| anyhow::anyhow!("nar_storage.get({hash}) failed: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("NAR not found in cache for {store_path}"))?;

    let total = bytes.len();
    if total == 0 {
        let _ = send_server_msg(
            socket,
            &ServerMessage::NarPush {
                job_id: job_id.to_owned(),
                store_path: store_path.to_owned(),
                data: Vec::new(),
                offset: 0,
                is_final: true,
            },
        )
        .await;
        return Ok(());
    }

    let mut offset: u64 = 0;
    let mut chunks = bytes.chunks(NAR_PUSH_CHUNK_SIZE).peekable();
    while let Some(chunk) = chunks.next() {
        let is_final = chunks.peek().is_none();
        let chunk_len = chunk.len() as u64;
        if send_server_msg(
            socket,
            &ServerMessage::NarPush {
                job_id: job_id.to_owned(),
                store_path: store_path.to_owned(),
                data: chunk.to_vec(),
                offset,
                is_final,
            },
        )
        .await
        .is_err()
        {
            return Err(anyhow::anyhow!(
                "WebSocket send failed mid-NarPush at offset {offset}"
            ));
        }
        offset += chunk_len;
    }
    debug!(
        %store_path,
        bytes = total,
        chunks = total.div_ceil(NAR_PUSH_CHUNK_SIZE),
        "NarRequest served"
    );
    Ok(())
}

// ── Credential delivery ───────────────────────────────────────────────────────

pub(super) async fn send_credentials_for_job(
    socket: &mut ProtoSocket,
    state: &ServerState,
    scheduler: &scheduler::Scheduler,
    worker_id: &str,
    job: &gradient_core::types::proto::Job,
    org_id: Uuid,
) {
    use gradient_core::types::proto::{FlakeTask, Job};

    let caps = scheduler.worker_gradient_caps(worker_id).await;
    let worker_can_fetch = caps.as_ref().map(|c| c.fetch).unwrap_or(false);
    let worker_can_sign = caps.as_ref().map(|c| c.sign).unwrap_or(false);

    match job {
        Job::Flake(flake_job) => {
            if worker_can_fetch && flake_job.tasks.contains(&FlakeTask::FetchFlake) {
                send_ssh_key_credential(socket, state, org_id).await;
            }
            if worker_can_sign {
                send_signing_key_credentials(socket, state, org_id).await;
            }
        }
        Job::Build(_) => {
            if worker_can_sign {
                send_signing_key_credentials(socket, state, org_id).await;
            }
        }
        Job::Sign(_) => {
            // Sign jobs require at least one signing key; if the worker
            // has no `sign` capability the scheduler shouldn't have
            // routed the job here, but guard anyway.
            if worker_can_sign {
                send_signing_key_credentials(socket, state, org_id).await;
            }
        }
    }
}

async fn send_ssh_key_credential(socket: &mut ProtoSocket, state: &ServerState, org_id: Uuid) {
    use gradient_core::types::proto::CredentialKind;

    match EOrganization::find_by_id(org_id).one(&state.db).await {
        Ok(Some(org)) => {
            match gradient_core::sources::ssh_key::decrypt_ssh_private_key(
                state.cli.crypt_secret_file.clone(),
                org,
                &state.cli.serve_url,
            ) {
                Ok((private_key, _public_key)) => {
                    let _ = send_server_msg(
                        socket,
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

/// Send one `Credential { SigningKey }` per cache in the org that has a
/// private key configured. The worker accumulates them and signs each
/// uploaded path once per key.
async fn send_signing_key_credentials(
    socket: &mut ProtoSocket,
    state: &ServerState,
    org_id: Uuid,
) {
    use gradient_core::sources::format_cache_key;
    use gradient_core::types::proto::CredentialKind;

    let org_caches = match EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .all(&state.db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(%org_id, error = %e, "failed to fetch org caches for signing keys");
            return;
        }
    };

    let mut sent = 0usize;
    for oc in org_caches {
        let cache = match ECache::find_by_id(oc.cache).one(&state.db).await {
            Ok(Some(c)) if !c.private_key.is_empty() => c,
            Ok(_) => continue,
            Err(e) => {
                warn!(error = %e, "failed to fetch cache for signing key");
                continue;
            }
        };

        match format_cache_key(
            state.cli.crypt_secret_file.clone(),
            cache.clone(),
            state.cli.serve_url.clone(),
        ) {
            Ok(key_str) => {
                let _ = send_server_msg(
                    socket,
                    &ServerMessage::Credential {
                        kind: CredentialKind::SigningKey,
                        data: key_str.into_bytes(),
                    },
                )
                .await;
                debug!(cache_name = %cache.name, %org_id, "signing key credential sent");
                sent += 1;
            }
            Err(e) => {
                warn!(cache_name = %cache.name, %org_id, error = %e, "failed to decrypt signing key");
            }
        }
    }

    if sent == 0 {
        debug!(%org_id, "no cache with signing key found for org");
    }
}
