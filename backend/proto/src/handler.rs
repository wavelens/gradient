/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::IntoResponse;
use axum::routing::get;
use gradient_core::types::ServerState;
use rkyv::rancor::Error as RkyvError;
use tracing::{debug, error, info, instrument, warn};

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

    let (client_version, client_capabilities, peer_id, token) = match init_msg {
        ClientMessage::InitConnection { version, capabilities, id, token } => {
            (version, capabilities, id, token)
        }
        _ => {
            send_error(&mut socket, 400, "expected InitConnection".into()).await;
            return;
        }
    };

    debug!(client_version, ?client_capabilities, %peer_id, "InitConnection received");

    if client_version > PROTO_VERSION {
        send_reject(
            &mut socket,
            400,
            format!("unsupported protocol version {client_version}"),
        )
        .await;
        return;
    }

    // TODO: validate token against configured API keys.
    let _ = token;

    let negotiated = negotiate_capabilities(&state, client_capabilities);

    if send_server_msg(
        &mut socket,
        &ServerMessage::InitAck { version: PROTO_VERSION, capabilities: negotiated.clone() },
    )
    .await
    .is_err()
    {
        return;
    }

    info!(%peer_id, client_version, "handshake complete");

    // Register this worker in the scheduler.
    scheduler.register_worker(&peer_id, negotiated.clone()).await;

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
                let candidates = scheduler.get_job_candidates().await;
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
                        // TODO: evaluation.status = Fetching
                    }
                    JobUpdateKind::EvaluatingFlake => {
                        // TODO: evaluation.status = EvaluatingFlake
                    }
                    JobUpdateKind::EvaluatingDerivations => {
                        // TODO: evaluation.status = EvaluatingDerivation
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
                        // TODO: build.status = Building
                        let _ = build_id;
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
