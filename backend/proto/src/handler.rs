/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use gradient_core::types::ServerState;
use rkyv::rancor::Error as RkyvError;
use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

use crate::messages::{
    CandidateScore, ClientMessage, GradientCapabilities, JobCandidate, JobUpdateKind, PROTO_VERSION,
    ServerMessage,
};

/// Returns the axum [`Router`] that serves the `/proto` WebSocket endpoint.
///
/// Merge this into the main router in `web`:
/// ```ignore
/// app.merge(proto::proto_router())
/// ```
pub fn proto_router() -> Router<Arc<ServerState>> {
    Router::new().route("/proto", get(ws_upgrade))
}

/// HTTP GET `/proto` — upgrades the connection to a WebSocket.
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Drives a single WebSocket connection for its lifetime.
///
/// Protocol state machine:
/// ```text
/// OPEN → [discoverable check] → HANDSHAKE → [token/version check]
///      → ACK → [optional WorkerCapabilities] → DISPATCH LOOP → CLOSED
/// ```
#[instrument(skip_all)]
async fn handle_socket(mut socket: WebSocket, state: Arc<ServerState>) {
    info!("WebSocket connection opened");

    if !state.cli.discoverable {
        send_reject(&mut socket, 403, "server is not accepting connections".into()).await;
        return;
    }

    // ── Handshake ─────────────────────────────────────────────────────────────

    let Some(init_msg) = recv_client_msg(&mut socket).await else {
        return;
    };

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
    // Cache-only connections (public caches) may omit the token.
    // let needs_auth = client_capabilities.fetch
    //     || client_capabilities.eval
    //     || client_capabilities.build
    //     || client_capabilities.sign
    //     || client_capabilities.federate;
    // if needs_auth && !validate_token(&state, token.as_deref()) {
    //     send_reject(&mut socket, 401, "invalid token".into()).await;
    //     return;
    // }
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

    // ── Dispatch loop ─────────────────────────────────────────────────────────

    // Tracks whether this peer has sent WorkerCapabilities (required before
    // RequestJobChunk / AssignJobResponse for build-capable workers).
    let mut worker_caps_received = false;

    loop {
        let msg = match recv_client_msg(&mut socket).await {
            Some(m) => m,
            None => break,
        };

        debug!(?msg, "received client message");

        match msg {
            // ── Already handled above ─────────────────────────────────────
            ClientMessage::InitConnection { .. } => {
                send_error(&mut socket, 400, "unexpected InitConnection".into()).await;
                break;
            }

            // ── Worker declines the connection ────────────────────────────
            ClientMessage::Reject { code, reason } => {
                info!(%peer_id, code, %reason, "peer rejected connection");
                break;
            }

            // ── Capability advertisement ──────────────────────────────────
            ClientMessage::WorkerCapabilities { architectures, system_features, max_concurrent_builds } => {
                debug!(
                    %peer_id,
                    ?architectures,
                    ?system_features,
                    max_concurrent_builds,
                    "WorkerCapabilities received"
                );
                worker_caps_received = true;
                // TODO: store worker capabilities in shared state for scheduler
            }

            // ── Job list snapshot ─────────────────────────────────────────
            ClientMessage::RequestJobList => {
                debug!(%peer_id, "RequestJobList");
                // TODO: query pending job candidates from DB/scheduler and
                //       stream them in chunks (e.g. 100 per message).
                //       For now send an empty final chunk.
                let chunk = ServerMessage::JobListChunk {
                    candidates: vec![],
                    is_final: true,
                };
                if send_server_msg(&mut socket, &chunk).await.is_err() {
                    break;
                }
            }

            // ── Incremental scoring ───────────────────────────────────────
            ClientMessage::RequestJobChunk { scores, is_final } => {
                debug!(%peer_id, count = scores.len(), is_final, "RequestJobChunk");
                // TODO: feed scores into scheduler; it may immediately
                //       assign a job if a score of `missing: 0` arrives.
                //
                // Example:
                // if let Some(assignment) = scheduler.consider_scores(&peer_id, &scores) {
                //     if send_server_msg(&mut socket, &ServerMessage::AssignJob {
                //         job_id: assignment.job_id,
                //         job: assignment.job,
                //         timeout_secs: assignment.timeout_secs,
                //     }).await.is_err() { break; }
                // }
                let _ = (scores, is_final);
            }

            // ── Job accept / reject ───────────────────────────────────────
            ClientMessage::AssignJobResponse { job_id, accepted, reason } => {
                if accepted {
                    info!(%peer_id, %job_id, "job accepted");
                    // TODO: mark job as Building in DB; start NAR transfer
                } else {
                    info!(%peer_id, %job_id, reason = ?reason, "job rejected by worker");
                    // TODO: release job back to scheduler for reassignment
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
                        // TODO: insert derivation/build rows, mark Substituted,
                        //       queue signing, push JobOffer to build workers.
                        //       First batch sets evaluation.status = Building.
                        let _ = (derivations, warnings);
                    }
                    JobUpdateKind::Building { build_id } => {
                        // TODO: build.status = Building
                        let _ = build_id;
                    }
                    JobUpdateKind::BuildOutput { build_id, outputs } => {
                        // TODO: build.status = Completed, update derivation_output rows
                        let _ = (build_id, outputs);
                    }
                    JobUpdateKind::Compressing => {
                        // Informational — no DB status change
                    }
                    JobUpdateKind::Signing => {
                        // Informational — no DB status change
                    }
                }
            }

            // ── Job terminal states ───────────────────────────────────────
            ClientMessage::JobCompleted { job_id } => {
                info!(%peer_id, %job_id, "job completed");
                // TODO: set terminal status (Completed / Succeeded) in DB;
                //       cascade to dependent builds; finalize log.
            }

            ClientMessage::JobFailed { job_id, error } => {
                warn!(%peer_id, %job_id, %error, "job failed");
                // TODO: set Failed in DB; cascade DependencyFailed to
                //       downstream builds; finalize log.
            }

            // ── Worker draining ───────────────────────────────────────────
            ClientMessage::Draining => {
                info!(%peer_id, "worker draining — no new jobs will be assigned");
                // TODO: mark peer as draining in scheduler state
            }

            // ── Log streaming ─────────────────────────────────────────────
            ClientMessage::LogChunk { job_id, task_index, data } => {
                debug!(%peer_id, %job_id, task_index, bytes = data.len(), "LogChunk");
                // TODO: append to LogStorage
                let _ = data;
            }

            // ── NAR transfer ──────────────────────────────────────────────
            ClientMessage::NarRequest { job_id, paths } => {
                debug!(%peer_id, %job_id, count = paths.len(), "NarRequest");
                // TODO: look up each path in NarStore; respond with NarPush
                //       (direct mode) or PresignedDownload (S3 mode).
                let _ = paths;
            }

            ClientMessage::NarPush { job_id, store_path, data, offset, is_final } => {
                debug!(%peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");
                // TODO: write chunk to NarStore; on is_final, verify hash,
                //       import into local store, record path info.
                let _ = data;
            }

            ClientMessage::NarReady { job_id, store_path, nar_size, nar_hash } => {
                debug!(%peer_id, %job_id, %store_path, nar_size, %nar_hash, "NarReady");
                // TODO: record path info; add_signatures if needed.
            }
        }
    }

    info!(%peer_id, "WebSocket connection closed");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Receive the next binary client message, skipping text/ping/pong frames.
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
            Ok(_) => continue, // text frames, pings, pongs — ignore
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

/// Negotiate capabilities: server-authoritative fields are set from server
/// config; all others are AND-ed with what the client requested.
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
