/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::IntoResponse;
use axum::routing::get;
use gradient_core::types::ServerState;
use rkyv::rancor::Error as RkyvError;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::messages::{
    ClientMessage, GradientCapabilities, JobUpdateKind, PROTO_VERSION, ServerMessage,
};
use crate::scheduler::Scheduler;

/// Returns the axum [`Router`] that serves the `/proto` WebSocket endpoint.
///
/// The caller must add `Extension<Arc<Scheduler>>` to the router layers
/// (typically done in `web::create_router`).
pub fn proto_router() -> Router<Arc<ServerState>> {
    Router::new().route("/proto", get(ws_upgrade))
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, scheduler))
}

/// Drives a single WebSocket connection for its lifetime.
///
/// Protocol state machine:
/// ```text
/// OPEN → [discoverable check] → HANDSHAKE → [version/token check]
///      → ACK → [optional WorkerCapabilities] → DISPATCH LOOP → CLOSED
/// ```
#[instrument(skip_all)]
async fn handle_socket(mut socket: WebSocket, state: Arc<ServerState>, scheduler: Arc<Scheduler>) {
    info!("WebSocket connection opened");

    if !state.cli.discoverable {
        send_reject(&mut socket, 403, "server is not accepting connections".into()).await;
        return;
    }

    // ── Handshake ─────────────────────────────────────────────────────────────

    let Some(init_msg) = recv_client_msg(&mut socket).await else { return };

    let (client_version, client_capabilities, peer_id) = match init_msg {
        ClientMessage::InitConnection { version, capabilities, id } => {
            (version, capabilities, id)
        }
        _ => {
            send_error(&mut socket, 400, "expected InitConnection".into()).await;
            return;
        }
    };

    debug!(client_version, ?client_capabilities, %peer_id, "InitConnection received");

    if client_version != PROTO_VERSION {
        send_reject(
            &mut socket,
            400,
            format!("unsupported protocol version {client_version}"),
        )
        .await;
        return;
    }

    // ── Challenge-response auth ───────────────────────────────────────────────

    // Look up which peers have registered this worker ID.
    let registered_peers = lookup_registered_peers(&state, &peer_id).await;

    if send_server_msg(
        &mut socket,
        &ServerMessage::AuthChallenge { peers: registered_peers.iter().map(|(id, _)| id.clone()).collect() },
    )
    .await
    .is_err()
    {
        return;
    }

    let auth_response = match recv_client_msg(&mut socket).await {
        Some(ClientMessage::AuthResponse { tokens }) => tokens,
        Some(_) => {
            send_error(&mut socket, 400, "expected AuthResponse".into()).await;
            return;
        }
        None => return,
    };

    let (authorized_peers, failed_peers) =
        validate_tokens(&registered_peers, &auth_response);

    // Require at least one authorized peer unless no peers are registered
    // (open/discoverable mode with no registrations).
    if registered_peers.is_empty() {
        debug!(%peer_id, "no registered peers — open connection accepted");
    } else if authorized_peers.is_empty() {
        send_reject(&mut socket, 401, "no valid peer tokens provided".into()).await;
        return;
    }

    let negotiated = negotiate_capabilities(&state, client_capabilities);

    if send_server_msg(
        &mut socket,
        &ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: negotiated.clone(),
            authorized_peers: authorized_peers.clone(),
            failed_peers: failed_peers.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }

    info!(%peer_id, client_version, authorized = authorized_peers.len(), "handshake complete");

    // Reject duplicate connections (same worker ID already connected).
    if scheduler.is_worker_connected(&peer_id).await {
        warn!(%peer_id, "duplicate connection rejected (worker already connected)");
        send_reject(&mut socket, 496, "worker already connected".into()).await;
        return;
    }

    // Parse authorized peer IDs as UUIDs for job dispatch filtering.
    let authorized_peer_uuids: HashSet<Uuid> = authorized_peers
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Register this worker in the scheduler.
    scheduler
        .register_worker(&peer_id, negotiated.clone(), authorized_peer_uuids)
        .await;

    // ── Dispatch loop ─────────────────────────────────────────────────────────

    loop {
        let msg = match recv_client_msg(&mut socket).await {
            Some(m) => m,
            None => break,
        };

        debug!(?msg, "received client message");

        match msg {
            ClientMessage::InitConnection { .. } => {
                send_error(&mut socket, 400, "unexpected InitConnection".into()).await;
                break;
            }

            ClientMessage::Reject { code, reason } => {
                info!(%peer_id, code, %reason, "peer rejected connection");
                break;
            }

            // ── Reauth ────────────────────────────────────────────────────
            ClientMessage::ReauthRequest => {
                debug!(%peer_id, "ReauthRequest");
                let registered_peers = lookup_registered_peers(&state, &peer_id).await;
                if send_server_msg(
                    &mut socket,
                    &ServerMessage::AuthChallenge {
                        peers: registered_peers.iter().map(|(id, _)| id.clone()).collect(),
                    },
                )
                .await
                .is_err()
                {
                    break;
                }
                // Await AuthResponse in the next iteration (client sends it immediately).
            }

            ClientMessage::AuthResponse { tokens } => {
                // Mid-connection reauth response.
                let registered_peers = lookup_registered_peers(&state, &peer_id).await;
                let (authorized_peers, failed_peers) =
                    validate_tokens(&registered_peers, &tokens);

                // Update the scheduler's peer filter for this worker.
                let updated_uuids: HashSet<Uuid> = authorized_peers
                    .iter()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                scheduler.update_authorized_peers(&peer_id, updated_uuids).await;

                if send_server_msg(
                    &mut socket,
                    &ServerMessage::AuthUpdate { authorized_peers, failed_peers },
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            // ── Capability advertisement ──────────────────────────────────
            ClientMessage::WorkerCapabilities { architectures, system_features, max_concurrent_builds } => {
                debug!(%peer_id, ?architectures, ?system_features, max_concurrent_builds, "WorkerCapabilities");
                scheduler
                    .update_worker_capabilities(&peer_id, architectures, system_features, max_concurrent_builds)
                    .await;
            }

            // ── Job list snapshot ─────────────────────────────────────────
            ClientMessage::RequestJobList => {
                debug!(%peer_id, "RequestJobList");
                let candidates = scheduler.get_job_candidates(&peer_id).await;
                let is_final = true;
                let chunk = ServerMessage::JobListChunk { candidates, is_final };
                if send_server_msg(&mut socket, &chunk).await.is_err() {
                    break;
                }
            }

            // ── Incremental scoring ───────────────────────────────────────
            ClientMessage::RequestJobChunk { scores, is_final } => {
                debug!(%peer_id, count = scores.len(), is_final, "RequestJobChunk");
                if let Some(assignment) = scheduler.consider_scores(&peer_id, scores).await {
                    let msg = ServerMessage::AssignJob {
                        job_id: assignment.job_id,
                        job: assignment.job,
                        timeout_secs: Some(3600),
                    };
                    if send_server_msg(&mut socket, &msg).await.is_err() {
                        break;
                    }
                }
            }

            // ── Job accept / reject ───────────────────────────────────────
            ClientMessage::AssignJobResponse { job_id, accepted, reason } => {
                if accepted {
                    info!(%peer_id, %job_id, "job accepted");
                    // Nothing extra needed — scheduler already marked it active.
                } else {
                    info!(%peer_id, %job_id, reason = ?reason, "job rejected by worker");
                    scheduler.job_rejected(&peer_id, &job_id).await;
                }
            }

            // ── Progress updates ──────────────────────────────────────────
            ClientMessage::JobUpdate { job_id, update } => {
                debug!(%peer_id, %job_id, ?update, "JobUpdate");
                match update {
                    JobUpdateKind::Fetching => {
                        scheduler.handle_eval_status_update(
                            &job_id,
                            entity::evaluation::EvaluationStatus::Fetching,
                        ).await;
                    }
                    JobUpdateKind::EvaluatingFlake => {
                        scheduler.handle_eval_status_update(
                            &job_id,
                            entity::evaluation::EvaluationStatus::EvaluatingFlake,
                        ).await;
                    }
                    JobUpdateKind::EvaluatingDerivations => {
                        scheduler.handle_eval_status_update(
                            &job_id,
                            entity::evaluation::EvaluationStatus::EvaluatingDerivation,
                        ).await;
                    }
                    JobUpdateKind::EvalResult { derivations, warnings } => {
                        if let Err(e) = scheduler
                            .handle_eval_result(&job_id, derivations, warnings)
                            .await
                        {
                            error!(%peer_id, %job_id, error = %e, "handle_eval_result failed");
                        }
                    }
                    JobUpdateKind::Building { build_id } => {
                        scheduler.handle_build_status_update(&build_id).await;
                    }
                    JobUpdateKind::BuildOutput { build_id, outputs } => {
                        if let Err(e) = scheduler
                            .handle_build_output(&job_id, &build_id, outputs)
                            .await
                        {
                            error!(%peer_id, %job_id, error = %e, "handle_build_output failed");
                        }
                    }
                    JobUpdateKind::Compressing | JobUpdateKind::Signing => {
                        // Informational — no DB status change needed.
                    }
                }
            }

            // ── Job terminal states ───────────────────────────────────────
            ClientMessage::JobCompleted { job_id } => {
                info!(%peer_id, %job_id, "job completed");
                if let Err(e) = scheduler.handle_job_completed(&peer_id, &job_id).await {
                    error!(%peer_id, %job_id, error = %e, "handle_job_completed failed");
                }
            }

            ClientMessage::JobFailed { job_id, error } => {
                warn!(%peer_id, %job_id, %error, "job failed");
                if let Err(e) = scheduler.handle_job_failed(&peer_id, &job_id, &error).await {
                    error!(%peer_id, %job_id, error = %e, "handle_job_failed failed");
                }
            }

            // ── Worker draining ───────────────────────────────────────────
            ClientMessage::Draining => {
                info!(%peer_id, "worker draining");
                scheduler.mark_worker_draining(&peer_id).await;
            }

            // ── Log streaming ─────────────────────────────────────────────
            ClientMessage::LogChunk { job_id, task_index, data } => {
                debug!(%peer_id, %job_id, task_index, bytes = data.len(), "LogChunk");
                if let Err(e) = scheduler.append_log(&job_id, task_index, data).await {
                    debug!(%peer_id, %job_id, error = %e, "log append failed");
                }
            }

            // ── NAR transfer ──────────────────────────────────────────────
            ClientMessage::NarRequest { job_id, paths } => {
                debug!(%peer_id, %job_id, count = paths.len(), "NarRequest");
                // TODO: look up each path in NarStore; respond NarPush or PresignedDownload.
            }

            ClientMessage::NarPush { job_id, store_path, data, offset, is_final } => {
                debug!(%peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");
                // TODO: write chunk to NarStore; on is_final verify hash + import.
            }

            ClientMessage::NarReady { job_id, store_path, nar_size, nar_hash } => {
                debug!(%peer_id, %job_id, %store_path, nar_size, %nar_hash, "NarReady");
                // TODO: record path info; add_signatures if needed.
            }
        }
    }

    scheduler.unregister_worker(&peer_id).await;
    info!(%peer_id, "WebSocket connection closed");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn recv_client_msg(socket: &mut WebSocket) -> Option<ClientMessage> {
    loop {
        match socket.recv().await? {
            Ok(Message::Binary(bytes)) => {
                match rkyv::from_bytes::<ClientMessage, RkyvError>(&bytes) {
                    Ok(msg) => return Some(msg),
                    Err(e) => {
                        warn!(error = %e, "failed to deserialize client message");
                        send_error(socket, 400, "malformed message".into()).await;
                        return None;
                    }
                }
            }
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(e) => {
                debug!(error = %e, "WebSocket recv error");
                return None;
            }
        }
    }
}

async fn send_server_msg(socket: &mut WebSocket, msg: &ServerMessage) -> Result<(), ()> {
    let bytes = rkyv::to_bytes::<RkyvError>(msg).map_err(|e| {
        warn!(error = %e, "failed to serialize server message");
    })?;
    socket
        .send(Message::Binary(bytes.to_vec().into()))
        .await
        .map_err(|e| debug!(error = %e, "WebSocket send error"))
}

async fn send_error(socket: &mut WebSocket, code: u16, message: String) {
    let _ = send_server_msg(socket, &ServerMessage::Error { code, message }).await;
}

async fn send_reject(socket: &mut WebSocket, code: u16, reason: String) {
    let _ = send_server_msg(socket, &ServerMessage::Reject { code, reason }).await;
}

/// Returns `(peer_id, token_hash)` pairs for all peers that registered this worker.
async fn lookup_registered_peers(state: &ServerState, worker_id: &str) -> Vec<(String, String)> {
    use entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .all(&state.db)
        .await
    {
        Ok(rows) => rows.into_iter().map(|r| (r.peer_id.to_string(), r.token_hash)).collect(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to look up registered peers");
            vec![]
        }
    }
}

/// Validates `auth_tokens` (worker-supplied `(peer_id, plaintext_token)`) against
/// `registered_peers` (`(peer_id, sha256_token_hash)`).
///
/// Returns `(authorized_peers, failed_peers)`.
fn validate_tokens(
    registered_peers: &[(String, String)],
    auth_tokens: &[(String, String)],
) -> (Vec<String>, Vec<crate::messages::FailedPeer>) {
    use sha2::{Digest, Sha256};

    let mut authorized = Vec::new();
    let mut failed = Vec::new();

    for (peer_id, token_hash) in registered_peers {
        match auth_tokens.iter().find(|(pid, _)| pid == peer_id) {
            Some((_, token)) => {
                let digest = hex::encode(Sha256::digest(token.as_bytes()));
                if digest == *token_hash {
                    authorized.push(peer_id.clone());
                } else {
                    failed.push(crate::messages::FailedPeer {
                        peer_id: peer_id.clone(),
                        reason: "invalid token".into(),
                    });
                }
            }
            None => {
                failed.push(crate::messages::FailedPeer {
                    peer_id: peer_id.clone(),
                    reason: "no token provided".into(),
                });
            }
        }
    }

    (authorized, failed)
}

fn negotiate_capabilities(state: &ServerState, client: GradientCapabilities) -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        cache: state.cli.serve_cache,
        federate: client.federate && state.cli.federate_proto,
        fetch: client.fetch,
        eval: client.eval,
        build: client.build,
        sign: client.sign,
    }
}
