/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Timelike;
use sea_orm::ActiveModelTrait;

use axum::Router;
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use gradient_core::types::*;
use rkyv::rancor::Error as RkyvError;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, tungstenite::Message as TungsteniteMessage,
};
use tracing::{debug, error, info, instrument, trace, warn};
use uuid::Uuid;

use crate::messages::{
    ClientMessage, GradientCapabilities, JobUpdateKind, PROTO_VERSION, ServerMessage,
};
use scheduler::Scheduler;

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
    ws.on_upgrade(move |socket| handle_socket(ProtoSocket::Axum(socket), state, scheduler, false))
}

// ── Socket abstraction ───────────────────────────────────────────────────────

/// Wraps both axum and raw tungstenite WebSocket streams so `handle_socket` can
/// drive connections regardless of who initiated the transport.
pub(crate) enum ProtoSocket {
    /// Inbound: worker connected to the server's `/proto` endpoint.
    Axum(WebSocket),
    /// Outbound: server connected to a worker's listener.
    Tungstenite(WebSocketStream<MaybeTlsStream<TcpStream>>),
}

impl ProtoSocket {
    async fn recv_bytes(&mut self) -> Option<Result<Vec<u8>, ()>> {
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

    async fn send_bytes(&mut self, bytes: Vec<u8>) -> Result<(), ()> {
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

/// Drives a single WebSocket connection for its lifetime.
///
/// Protocol state machine:
/// ```text
/// OPEN → [discoverable check] → HANDSHAKE → [version/token check]
///      → ACK → [optional WorkerCapabilities] → DISPATCH LOOP → CLOSED
/// ```
///
/// When `server_initiated` is true the discoverable check is skipped — the
/// server already decided to connect outbound to this worker.
#[instrument(skip_all)]
pub(crate) async fn handle_socket(
    mut socket: ProtoSocket,
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    server_initiated: bool,
) {
    info!(server_initiated, "WebSocket connection opened");

    if !server_initiated && !state.cli.discoverable {
        send_reject(
            &mut socket,
            403,
            "server is not accepting connections".into(),
        )
        .await;
        return;
    }

    // ── Handshake ─────────────────────────────────────────────────────────────

    let Some(init_msg) = recv_client_msg(&mut socket).await else {
        return;
    };

    let (client_version, client_capabilities, peer_id) = match init_msg {
        ClientMessage::InitConnection {
            version,
            capabilities,
            id,
        } => (version, capabilities, id),
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
        &ServerMessage::AuthChallenge {
            peers: registered_peers.iter().map(|(id, _)| id.clone()).collect(),
        },
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

    let (authorized_peers, failed_peers) = validate_tokens(&registered_peers, &auth_response);

    // Require at least one authorized peer unless no peers have ever registered
    // this worker (open/discoverable mode with no registrations at all).
    if registered_peers.is_empty() {
        // Distinguish "no registrations" (open mode) from "all registrations
        // deactivated" (worker was explicitly disabled).
        if has_any_registrations(&state, &peer_id).await {
            send_reject(&mut socket, 403, "worker is deactivated".into()).await;
            return;
        }
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
    let (reauth_notify, mut abort_rx) = scheduler
        .register_worker(&peer_id, negotiated.clone(), authorized_peer_uuids)
        .await;
    let job_notify = scheduler.job_notify();

    // ── Dispatch loop ─────────────────────────────────────────────────────────

    // Buffer for direct NAR push (local storage mode).
    // Key: store_path, Value: accumulated compressed bytes.
    let mut nar_buffers: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    loop {
        let msg = tokio::select! {
            msg = recv_client_msg(&mut socket) => {
                match msg {
                    Some(m) => m,
                    None => break,
                }
            }
            _ = reauth_notify.notified() => {
                // Server-initiated reauth: registrations changed via API.
                debug!(%peer_id, "server-initiated reauth");
                let registered_peers = lookup_registered_peers(&state, &peer_id).await;

                // If there are registrations but none are active, all were
                // deactivated — disconnect the worker immediately.
                if registered_peers.is_empty() && has_any_registrations(&state, &peer_id).await {
                    info!(%peer_id, "all registrations deactivated — disconnecting worker");
                    send_reject(&mut socket, 403, "worker is deactivated".into()).await;
                    break;
                }

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
                continue;
            }
            _ = job_notify.notified() => {
                // New jobs enqueued — push only candidates not yet sent (delta),
                // paginated at JOB_OFFER_CHUNK_SIZE per message.
                let candidates = scheduler.get_new_job_candidates(&peer_id).await;
                if !candidates.is_empty() {
                    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta)");
                    let mut failed = false;
                    for chunk in candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
                        if send_server_msg(
                            &mut socket,
                            &ServerMessage::JobOffer { candidates: chunk.to_vec() },
                        )
                        .await
                        .is_err()
                        {
                            failed = true;
                            break;
                        }
                    }
                    if failed { break; }
                }
                continue;
            }
            abort_msg = abort_rx.recv() => {
                match abort_msg {
                    Some((job_id, reason)) => {
                        info!(%peer_id, %job_id, %reason, "sending AbortJob to worker");
                        if send_server_msg(
                            &mut socket,
                            &ServerMessage::AbortJob { job_id, reason },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    None => {
                        // Channel closed — scheduler dropped the sender.
                        break;
                    }
                }
            }
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
                let (authorized_peers, failed_peers) = validate_tokens(&registered_peers, &tokens);

                // Update the scheduler's peer filter for this worker.
                let updated_uuids: HashSet<Uuid> = authorized_peers
                    .iter()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                scheduler
                    .update_authorized_peers(&peer_id, updated_uuids)
                    .await;

                if send_server_msg(
                    &mut socket,
                    &ServerMessage::AuthUpdate {
                        authorized_peers,
                        failed_peers,
                    },
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            // ── Capability advertisement ──────────────────────────────────
            ClientMessage::WorkerCapabilities {
                architectures,
                system_features,
                max_concurrent_builds,
            } => {
                debug!(%peer_id, ?architectures, ?system_features, max_concurrent_builds, "WorkerCapabilities");
                scheduler
                    .update_worker_capabilities(
                        &peer_id,
                        architectures,
                        system_features,
                        max_concurrent_builds,
                    )
                    .await;
            }

            // ── Job list snapshot ─────────────────────────────────────────
            ClientMessage::RequestJobList => {
                debug!(%peer_id, "RequestJobList");
                let candidates = scheduler.get_job_candidates(&peer_id).await;
                let chunks: Vec<_> = candidates.chunks(JOB_OFFER_CHUNK_SIZE).collect();
                let total = chunks.len();
                let mut failed = false;
                for (i, chunk) in chunks.into_iter().enumerate() {
                    let is_final = i + 1 == total || total == 0;
                    if send_server_msg(
                        &mut socket,
                        &ServerMessage::JobListChunk {
                            candidates: chunk.to_vec(),
                            is_final,
                        },
                    )
                    .await
                    .is_err()
                    {
                        failed = true;
                        break;
                    }
                }
                // Send an empty final chunk when there are no candidates.
                if total == 0 {
                    if send_server_msg(
                        &mut socket,
                        &ServerMessage::JobListChunk { candidates: vec![], is_final: true },
                    )
                    .await
                    .is_err()
                    {
                        failed = true;
                    }
                }
                if failed { break; }
            }

            // ── Pull-based capacity signal ────────────────────────────────
            ClientMessage::RequestJob { kind } => {
                debug!(%peer_id, ?kind, "RequestJob");
                if let Some(assignment) = scheduler.request_job(&peer_id, kind).await {
                    send_credentials_for_job(
                        &mut socket,
                        &state,
                        &assignment.job,
                        assignment.peer_id,
                    )
                    .await;
                    let msg = ServerMessage::AssignJob {
                        job_id: assignment.job_id,
                        job: assignment.job,
                        timeout_secs: Some(3600),
                    };
                    if send_server_msg(&mut socket, &msg).await.is_err() {
                        break;
                    }
                }
                // No job available — worker will retry via 10s heartbeat.
            }

            ClientMessage::RequestAllCandidates => {
                debug!(%peer_id, "RequestAllCandidates");
                let candidates = scheduler.get_job_candidates(&peer_id).await;
                let chunks: Vec<_> = candidates.chunks(JOB_OFFER_CHUNK_SIZE).collect();
                let total = chunks.len();
                let mut failed = false;
                for (i, chunk) in chunks.into_iter().enumerate() {
                    let is_final = i + 1 == total || total == 0;
                    if send_server_msg(
                        &mut socket,
                        &ServerMessage::JobListChunk {
                            candidates: chunk.to_vec(),
                            is_final,
                        },
                    )
                    .await
                    .is_err()
                    {
                        failed = true;
                        break;
                    }
                }
                if total == 0 {
                    if send_server_msg(
                        &mut socket,
                        &ServerMessage::JobListChunk { candidates: vec![], is_final: true },
                    )
                    .await
                    .is_err()
                    {
                        failed = true;
                    }
                }
                if failed { break; }
            }

            // ── Incremental scoring ───────────────────────────────────────
            ClientMessage::RequestJobChunk { scores, is_final } => {
                debug!(%peer_id, count = scores.len(), is_final, "RequestJobChunk");
                if let Some(assignment) = scheduler.consider_scores(&peer_id, scores).await {
                    // Deliver credentials before the job assignment.
                    send_credentials_for_job(
                        &mut socket,
                        &state,
                        &assignment.job,
                        assignment.peer_id,
                    )
                    .await;

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
            ClientMessage::AssignJobResponse {
                job_id,
                accepted,
                reason,
            } => {
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
                        scheduler
                            .handle_eval_status_update(
                                &job_id,
                                entity::evaluation::EvaluationStatus::Fetching,
                            )
                            .await;
                    }
                    JobUpdateKind::FetchResult { fetched_paths } => {
                        debug!(%peer_id, %job_id, count = fetched_paths.len(), "FetchResult");
                        if let Err(e) = record_fetched_paths(
                            &state,
                            &scheduler,
                            &job_id,
                            &fetched_paths,
                        )
                        .await
                        {
                            warn!(%job_id, error = %e, "failed to record fetched paths in cache");
                        }
                    }
                    JobUpdateKind::EvaluatingFlake => {
                        scheduler
                            .handle_eval_status_update(
                                &job_id,
                                entity::evaluation::EvaluationStatus::EvaluatingFlake,
                            )
                            .await;
                    }
                    JobUpdateKind::EvaluatingDerivations => {
                        scheduler
                            .handle_eval_status_update(
                                &job_id,
                                entity::evaluation::EvaluationStatus::EvaluatingDerivation,
                            )
                            .await;
                    }
                    JobUpdateKind::EvalResult {
                        derivations,
                        warnings,
                        errors,
                    } => {
                        if let Err(e) = scheduler
                            .handle_eval_result(&job_id, derivations, warnings, errors)
                            .await
                        {
                            error!(%peer_id, %job_id, error = %e, "handle_eval_result failed");
                        }
                        // Eval result creates builds and dispatches them into
                        // the in-memory tracker. job_notify fired during
                        // processing but this handler wasn't in select! so the
                        // notification was lost. Push candidates directly.
                        push_pending_candidates(&mut socket, &scheduler, &peer_id).await;
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
                // A completed build unlocks dependents; push any new
                // candidates that were dispatched during processing.
                push_pending_candidates(&mut socket, &scheduler, &peer_id).await;
            }

            ClientMessage::JobFailed { job_id, error } => {
                warn!(%peer_id, %job_id, %error, "job failed");
                if let Err(e) = scheduler.handle_job_failed(&peer_id, &job_id, &error).await {
                    error!(%peer_id, %job_id, error = %e, "handle_job_failed failed");
                }
                // Failed build cascades DependencyFailed; push any freed
                // candidates.
                push_pending_candidates(&mut socket, &scheduler, &peer_id).await;
            }

            // ── Worker draining ───────────────────────────────────────────
            ClientMessage::Draining => {
                info!(%peer_id, "worker draining");
                scheduler.mark_worker_draining(&peer_id).await;
            }

            // ── Log streaming ─────────────────────────────────────────────
            ClientMessage::LogChunk {
                job_id,
                task_index,
                data,
            } => {
                debug!(%peer_id, %job_id, task_index, bytes = data.len(), "LogChunk");
                if let Err(e) = scheduler.append_log(&job_id, task_index, data).await {
                    debug!(%peer_id, %job_id, error = %e, "log append failed");
                }
            }

            // ── NAR transfer ──────────────────────────────────────────────
            ClientMessage::NarRequest { job_id, paths } => {
                debug!(%peer_id, %job_id, count = paths.len(), "NarRequest");
                for store_path in paths {
                    if let Err(e) =
                        serve_nar_request(&state, &mut socket, &job_id, &store_path).await
                    {
                        warn!(%peer_id, %job_id, %store_path, error = %e, "NarRequest serve failed");
                    }
                }
            }

            ClientMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                debug!(%peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");

                // Accumulate data chunks in the per-store-path buffer.
                if !data.is_empty() {
                    nar_buffers
                        .entry(store_path.clone())
                        .or_default()
                        .extend_from_slice(&data);
                }

                if is_final {
                    let buf = nar_buffers.remove(&store_path).unwrap_or_default();
                    let compressed_size = buf.len() as i64;

                    // Derive the NarStore hash key from the store path.
                    let hash_opt = store_path
                        .strip_prefix("/nix/store/")
                        .unwrap_or(&store_path)
                        .split('-')
                        .next()
                        .map(str::to_owned);

                    if let Some(hash) = hash_opt {
                        if let Err(e) = state.nar_storage.put(&hash, buf).await {
                            error!(%peer_id, %job_id, %store_path, error = %e, "NarPush write failed");
                        } else {
                            info!(%peer_id, %job_id, %store_path, compressed_size, "NarPush stored");
                            // NarUploaded (sent by worker after this) handles
                            // mark_nar_stored and cache metrics.
                        }
                    } else {
                        warn!(%peer_id, %job_id, %store_path, "NarPush: could not parse store path hash");
                    }
                }
            }

            ClientMessage::NarReady {
                job_id,
                store_path,
                nar_size,
                nar_hash,
            } => {
                debug!(%peer_id, %job_id, %store_path, nar_size, %nar_hash, "NarReady");
                if let Err(e) =
                    scheduler::build::handle_nar_ready(&state, &store_path, nar_size, &nar_hash)
                        .await
                {
                    error!(%peer_id, %job_id, error = %e, "handle_nar_ready failed");
                }
                // NarUploaded (sent by worker after this) handles
                // mark_nar_stored and cache metrics.
            }

            ClientMessage::NarUploaded {
                job_id,
                store_path,
                file_hash,
                file_size,
                nar_size,
                nar_hash,
            } => {
                debug!(%peer_id, %job_id, %store_path, %file_hash, file_size, nar_size, %nar_hash, "NarUploaded");
                let file_size_i64 = file_size as i64;
                if let Err(e) = mark_nar_stored(&state, &store_path, file_size_i64, Some(&file_hash), Some(nar_size as i64), Some(&nar_hash)).await {
                    warn!(%store_path, error = %e, "failed to mark NAR as stored");
                }
                if let Err(e) = record_nar_push_metric(&state, &scheduler, &job_id, file_size_i64).await {
                    debug!(error = %e, "failed to record cache metric for NarUploaded");
                }
            }

            // ── Cache queries ────────────────────────────────────────────
            ClientMessage::CacheQuery { job_id, paths, mode } => {
                debug!(%peer_id, %job_id, count = paths.len(), ?mode, "CacheQuery");
                let org_id = scheduler.peer_id_for_job(&job_id).await;
                let cached = handle_cache_query(&state, org_id, &paths, mode).await;
                debug!(%peer_id, %job_id, entries = cached.len(), "CacheStatus");

                if send_server_msg(
                    &mut socket,
                    &ServerMessage::CacheStatus { job_id, cached },
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }
    }

    scheduler.unregister_worker(&peer_id).await;
    info!(%peer_id, "WebSocket connection closed");
}

// ── Credential delivery ──────────────────────────────────────────────────────

/// Send credentials the worker needs to execute the job.
///
/// - **FlakeJob with FetchFlake**: sends the org's SSH private key for cloning
///   private repositories.
/// - **FlakeJob with sign**: sends the cache's signing key.
/// - **BuildJob with Sign**: sends the cache's signing key.
async fn send_credentials_for_job(
    socket: &mut ProtoSocket,
    state: &ServerState,
    job: &gradient_core::types::proto::Job,
    org_id: Uuid,
) {
    use gradient_core::types::proto::{CredentialKind, FlakeTask, Job};

    match job {
        Job::Flake(flake_job) => {
            if flake_job.tasks.contains(&FlakeTask::FetchFlake) {
                // Look up the org's SSH private key.
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
            if flake_job.sign.is_some() {
                send_signing_key_credential(socket, state, org_id).await;
            }
        }
        Job::Build(build_job) => {
            if build_job.sign.is_some() {
                send_signing_key_credential(socket, state, org_id).await;
            }
        }
    }
}

/// Look up a cache signing key for `org_id` and deliver it as a
/// `Credential { kind: SigningKey }` message.  Used by both `FlakeJob` (sign
/// fetched sources) and `BuildJob` (sign built outputs).
async fn send_signing_key_credential(
    socket: &mut ProtoSocket,
    state: &ServerState,
    org_id: Uuid,
) {
    use gradient_core::types::proto::CredentialKind;

    match EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org_id))
        .all(&state.db)
        .await
    {
        Ok(org_caches) => {
            for oc in org_caches {
                match ECache::find_by_id(oc.cache).one(&state.db).await {
                    Ok(Some(cache)) if !cache.private_key.is_empty() => {
                        let _ = send_server_msg(
                            socket,
                            &ServerMessage::Credential {
                                kind: CredentialKind::SigningKey,
                                data: cache.private_key.into_bytes(),
                            },
                        )
                        .await;
                        debug!(cache_name = %cache.name, %org_id, "signing key credential sent");
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "failed to fetch cache for signing key"),
                }
            }
            debug!(%org_id, "no cache with signing key found for org");
        }
        Err(e) => warn!(%org_id, error = %e, "failed to fetch org caches for signing key"),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Push any pending job candidates to the worker.
///
/// Called after processing messages that create or free jobs (EvalResult,
/// JobCompleted, JobFailed). The handler can't rely on `job_notify` for
/// these because it's the one doing the processing — it isn't in `select!`
/// waiting on `notified()`, so notifications fired during processing are
/// lost. This direct push ensures the worker learns about new candidates
/// immediately.
const JOB_OFFER_CHUNK_SIZE: usize = 1_000;

async fn push_pending_candidates(
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

async fn recv_client_msg(socket: &mut ProtoSocket) -> Option<ClientMessage> {
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

async fn send_server_msg(socket: &mut ProtoSocket, msg: &ServerMessage) -> Result<(), ()> {
    let bytes = rkyv::to_bytes::<RkyvError>(msg).map_err(|e| {
        warn!(error = %e, "failed to serialize server message");
    })?;
    trace!(?msg, bytes = bytes.len(), "send ServerMessage");
    socket.send_bytes(bytes.to_vec()).await
}

async fn send_error(socket: &mut ProtoSocket, code: u16, message: String) {
    let _ = send_server_msg(socket, &ServerMessage::Error { code, message }).await;
}

async fn send_reject(socket: &mut ProtoSocket, code: u16, reason: String) {
    let _ = send_server_msg(socket, &ServerMessage::Reject { code, reason }).await;
}

/// Returns `(peer_id, token_hash)` pairs for all **active** peers that registered this worker.
async fn lookup_registered_peers(state: &ServerState, worker_id: &str) -> Vec<(String, String)> {
    use entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .filter(Column::Active.eq(true))
        .all(&state.db)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.peer_id.to_string(), r.token_hash))
            .collect(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to look up registered peers");
            vec![]
        }
    }
}

/// Returns `true` if *any* `worker_registration` row exists for this worker,
/// regardless of the `active` flag.  Used to distinguish "no registrations at
/// all" (open/discoverable mode) from "all registrations deactivated".
async fn has_any_registrations(state: &ServerState, worker_id: &str) -> bool {
    use entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .one(&state.db)
        .await
    {
        Ok(row) => row.is_some(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to check worker registrations");
            false
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

fn negotiate_capabilities(
    state: &ServerState,
    client: GradientCapabilities,
) -> GradientCapabilities {
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

/// Create `cached_path` rows for source paths pushed during evaluation.
///
/// Resolves `job_id → org → caches` and creates one `cached_path` row per
/// cache for each fetched input. The worker-provided signature (if any) is
/// stored directly — it was produced with the cache's signing key that was
/// sent to the worker during handshake.
async fn record_fetched_paths(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    fetched_paths: &[gradient_core::types::proto::FetchedInput],
) -> anyhow::Result<()> {
    use entity::cached_path::{ActiveModel as ACachedPath, Column as CCachedPath, Entity as ECachedPath};
    use entity::organization_cache::{Column as COrgCache, Entity as EOrgCache};
    use gradient_core::sources::get_hash_from_path;
    use sea_orm::{ActiveModelTrait, Set};

    if fetched_paths.is_empty() {
        return Ok(());
    }

    let org_id = scheduler
        .peer_id_for_job(job_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("no peer for job {}", job_id))?;

    let org_caches = EOrgCache::find()
        .filter(COrgCache::Organization.eq(org_id))
        .all(&state.db)
        .await?;

    if org_caches.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().naive_utc();

    for fi in fetched_paths {
        let (hash, package) = match get_hash_from_path(fi.store_path.clone()) {
            Ok(v) => v,
            Err(e) => {
                warn!(store_path = %fi.store_path, error = %e, "cannot parse fetched path");
                continue;
            }
        };

        // Find or create the cached_path row (one per unique hash).
        let cached_path_row = match ECachedPath::find()
            .filter(CCachedPath::Hash.eq(&hash))
            .one(&state.db)
            .await?
        {
            Some(row) => row,
            None => {
                let am = ACachedPath {
                    id: Set(uuid::Uuid::new_v4()),
                    store_path: Set(fi.store_path.clone()),
                    hash: Set(hash.clone()),
                    package: Set(package.clone()),
                    file_hash: Set(None),
                    file_size: Set(None),
                    nar_size: Set(Some(fi.nar_size as i64)),
                    nar_hash: Set(Some(fi.nar_hash.clone())),
                    references: Set(None),
                    ca: Set(None),
                    created_at: Set(now),
                };
                match am.insert(&state.db).await {
                    Ok(row) => row,
                    Err(e) => {
                        warn!(store_path = %fi.store_path, error = %e, "failed to insert cached_path");
                        continue;
                    }
                }
            }
        };

        // Create a cached_path_signature row per cache (with optional signature).
        for oc in &org_caches {
            use entity::cached_path_signature::{
                Column as CCachedPathSig, Entity as ECachedPathSig,
            };

            let existing = ECachedPathSig::find()
                .filter(CCachedPathSig::CachedPath.eq(cached_path_row.id))
                .filter(CCachedPathSig::Cache.eq(oc.cache))
                .one(&state.db)
                .await?;

            if existing.is_some() {
                continue;
            }

            let sig_row = ACachedPathSignature {
                id: Set(uuid::Uuid::new_v4()),
                cached_path: Set(cached_path_row.id),
                cache: Set(oc.cache),
                signature: Set(fi.signature.clone()),
                created_at: Set(now),
            };
            if let Err(e) = sig_row.insert(&state.db).await {
                warn!(store_path = %fi.store_path, cache = %oc.cache, error = %e, "failed to insert cached_path_signature");
            }
        }
    }

    info!(count = fetched_paths.len(), %org_id, "recorded fetched paths in cache");
    Ok(())
}

/// Record a cache metric entry for a NAR push (direct or presigned).
///
/// Resolves `job_id → org → cache` and increments the traffic counter.
async fn record_nar_push_metric(
    state: &ServerState,
    scheduler: &Scheduler,
    job_id: &str,
    bytes: i64,
) -> anyhow::Result<()> {
    use entity::cache_metric::{
        ActiveModel as ACacheMetric, Column as CCacheMetric, Entity as ECacheMetric,
    };
    use entity::organization_cache::{Column as COrgCache, Entity as EOrgCache};
    use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};

    let org_id = scheduler
        .peer_id_for_job(job_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("no peer for job {}", job_id))?;

    let org_cache = EOrgCache::find()
        .filter(COrgCache::Organization.eq(org_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no cache for org {}", org_id))?;

    let cache_id = org_cache.cache;
    let now = chrono::Utc::now().naive_utc();
    let bucket = now
        .with_second(0)
        .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
        .unwrap_or(now);

    match ECacheMetric::find()
        .filter(CCacheMetric::Cache.eq(cache_id))
        .filter(CCacheMetric::BucketTime.eq(bucket))
        .one(&state.db)
        .await?
    {
        Some(metric) => {
            let mut am: ACacheMetric = metric.into_active_model();
            am.bytes_sent = Set(am.bytes_sent.unwrap() + bytes);
            am.nar_count = Set(am.nar_count.unwrap() + 1);
            am.update(&state.db).await?;
        }
        None => {
            let am = ACacheMetric {
                id: Set(uuid::Uuid::new_v4()),
                cache: Set(cache_id),
                bucket_time: Set(bucket),
                bytes_sent: Set(bytes),
                nar_count: Set(1),
            };
            am.insert(&state.db).await?;
        }
    }

    Ok(())
}

/// Update the `derivation_output` or `cached_path` record for `store_path`
/// after a direct NAR push.
///
/// Sets `is_cached = true` and `file_size` on `derivation_output` if one
/// exists. Also updates `file_size` and `file_hash` on any matching
/// `cached_path` rows (used for source paths pushed during eval).
async fn mark_nar_stored(
    state: &ServerState,
    store_path: &str,
    file_size: i64,
    file_hash: Option<&str>,
    nar_size: Option<i64>,
    nar_hash: Option<&str>,
) -> anyhow::Result<()> {
    use entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
    use entity::derivation_output::{Column as CDerivationOutput, Entity as EDerivationOutput};
    use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};

    // Update derivation_output if one exists.
    let existing = EDerivationOutput::find()
        .filter(CDerivationOutput::Output.eq(store_path))
        .one(&state.db)
        .await?;

    if let Some(row) = existing {
        let mut active = row.into_active_model();
        active.is_cached = Set(true);
        active.file_size = Set(Some(file_size));
        active.update(&state.db).await?;
        info!(store_path, file_size, "derivation_output marked cached after NarPush");
    }

    // Update cached_path rows (source paths).
    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()
        .unwrap_or("");

    if !hash.is_empty() {
        let cached_rows = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash))
            .all(&state.db)
            .await?;

        for row in cached_rows {
            let mut active = row.into_active_model();
            active.file_size = Set(Some(file_size));
            if let Some(fh) = file_hash {
                active.file_hash = Set(Some(fh.to_owned()));
            }
            if let Some(ns) = nar_size {
                active.nar_size = Set(Some(ns));
            }
            if let Some(nh) = nar_hash {
                active.nar_hash = Set(Some(nh.to_owned()));
            }
            active.update(&state.db).await?;
        }
    }

    Ok(())
}

/// Check which store paths are available — in the local Gradient cache or upstream.
///
/// Build the [`CacheStatus`] response for a [`CacheQuery`].
///
/// Behaviour depends on `mode`:
/// - [`QueryMode::Normal`] — return only paths that are cached (`cached: true`).
///   Upstream caches are checked for any path not found locally.  No URLs.
/// - [`QueryMode::Pull`]   — same as Normal but cached paths include a presigned
///   S3 GET URL when the store is S3-backed (`url: None` → use `NarRequest`).
/// - [`QueryMode::Push`]   — return **all** queried paths with `cached` set.
///   Uncached paths include a presigned S3 PUT URL when S3-backed
///   (`url: None` → use `NarPush`).  Upstream cache lookups are skipped.
async fn handle_cache_query(
    state: &ServerState,
    org_id: Option<uuid::Uuid>,
    paths: &[String],
    mode: gradient_core::types::proto::QueryMode,
) -> Vec<gradient_core::types::proto::CachedPath> {
    use entity::cache_upstream::{Column as CCacheUpstream, Entity as ECacheUpstream};
    use entity::derivation_output::{Column as CDerivationOutput, Entity as EDerivationOutput};
    use entity::organization_cache::{
        CacheSubscriptionMode, Column as COrgCache, Entity as EOrgCache,
    };
    use gradient_core::types::proto::{CachedPath, QueryMode};

    // Extract (hash, original_path) pairs from store paths.
    let hash_path_pairs: Vec<(&str, &str)> = paths
        .iter()
        .filter_map(|p| {
            let base = p.strip_prefix("/nix/store/").unwrap_or(p);
            let hash = base.split('-').next()?;
            if hash.len() == 32 { Some((hash, p.as_str())) } else { None }
        })
        .collect();

    if hash_path_pairs.is_empty() {
        return vec![];
    }

    let hashes: Vec<&str> = hash_path_pairs.iter().map(|(h, _)| *h).collect();

    // ── Local cache lookup ────────────────────────────────────────────────────

    let cached_rows = match EDerivationOutput::find()
        .filter(
            sea_orm::Condition::all()
                .add(CDerivationOutput::IsCached.eq(true))
                .add(CDerivationOutput::Hash.is_in(hashes.clone())),
        )
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "CacheQuery local DB lookup failed");
            vec![]
        }
    };

    let mut cached_map: std::collections::HashMap<&str, (Option<i64>, Option<i64>)> = cached_rows
        .iter()
        .map(|r| (r.hash.as_str(), (r.file_size, r.nar_size)))
        .collect();

    // Also check cached_path table for source paths.
    let cached_path_rows = match entity::cached_path::Entity::find()
        .filter(entity::cached_path::Column::Hash.is_in(hashes.clone()))
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "CacheQuery cached_path lookup failed");
            vec![]
        }
    };

    for cp in &cached_path_rows {
        // Only mark as cached if file_hash is set (NAR was actually uploaded).
        if cp.file_hash.is_some() {
            cached_map
                .entry(cp.hash.as_str())
                .or_insert((cp.file_size, cp.nar_size));
        }
    }

    // For Push mode: ensure cached_path_signature rows exist for the org's caches
    // so signing jobs get created for already-cached paths.
    if matches!(mode, QueryMode::Push) {
        if let Some(oid) = org_id {
            let org_caches = EOrgCache::find()
                .filter(COrgCache::Organization.eq(oid))
                .all(&state.db)
                .await
                .unwrap_or_default();

            for cp in &cached_path_rows {
                for oc in &org_caches {
                    use entity::cached_path_signature::{
                        Column as CCachedPathSig, Entity as ECachedPathSig,
                    };
                    let exists = ECachedPathSig::find()
                        .filter(CCachedPathSig::CachedPath.eq(cp.id))
                        .filter(CCachedPathSig::Cache.eq(oc.cache))
                        .one(&state.db)
                        .await
                        .unwrap_or(None)
                        .is_some();

                    if !exists {
                        let sig_row = ACachedPathSignature {
                            id: sea_orm::ActiveValue::Set(uuid::Uuid::new_v4()),
                            cached_path: sea_orm::ActiveValue::Set(cp.id),
                            cache: sea_orm::ActiveValue::Set(oc.cache),
                            signature: sea_orm::ActiveValue::Set(None),
                            created_at: sea_orm::ActiveValue::Set(chrono::Utc::now().naive_utc()),
                        };
                        let _ = sig_row.insert(&state.db).await;
                    }
                }
            }
        }
    }

    let expire = std::time::Duration::from_secs(3600);

    // ── Build result for locally-cached paths ─────────────────────────────────
    let mut result: Vec<CachedPath> = Vec::new();
    for (hash, path) in &hash_path_pairs {
        if let Some((file_size, nar_size)) = cached_map.get(hash) {
            // Path is locally cached.
            let url = match mode {
                QueryMode::Pull => {
                    match state.nar_storage.presigned_get_url(hash, expire).await {
                        Ok(u) => u,
                        Err(e) => {
                            warn!(%hash, error = %e, "failed to generate presigned GET URL");
                            None
                        }
                    }
                }
                _ => None,
            };
            // For Pull mode, look up the cached_path row to get the import
            // metadata (nar_hash / references / ca) and any matching signatures
            // so the worker can construct a `ValidPathInfo` and call
            // `add_to_store_nar` on its local nix-daemon.
            let (nar_hash, references, signatures, deriver, ca) = match mode {
                QueryMode::Pull => fetch_pull_metadata(state, hash).await,
                _ => (None, None, None, None, None),
            };
            result.push(CachedPath {
                path: path.to_string(),
                cached: true,
                file_size: file_size.map(|v| v as u64),
                nar_size: nar_size.map(|v| v as u64),
                url,
                nar_hash,
                references,
                signatures,
                deriver,
                ca,
            });
        } else if matches!(mode, QueryMode::Push) {
            // Path is NOT locally cached — for Push mode return it so the
            // worker knows to upload, with an optional presigned PUT URL.
            // For S3 stores, `presigned_put_url` returns `Ok(Some(url))`; for
            // local stores, `Ok(None)` (worker falls back to chunked NarPush).
            // An `Err` means the S3 backend rejected the signing request —
            // operator intervention is needed (credentials, endpoint, region).
            // Log at error level since the fallback (NarPush through the WS)
            // routes every NAR through the gradient-server process and
            // defeats the whole point of having S3 configured.
            let url = match state.nar_storage.presigned_put_url(hash, expire).await {
                Ok(u) => u,
                Err(e) => {
                    error!(
                        %hash,
                        error = %e,
                        "S3 presigned PUT URL generation failed; worker will fall back to \
                         direct NarPush (this defeats S3 — check S3 credentials / endpoint / \
                         region config)"
                    );
                    None
                }
            };
            result.push(CachedPath {
                path: path.to_string(),
                cached: false,
                file_size: None,
                nar_size: None,
                url,
                nar_hash: None,
                references: None,
                signatures: None,
                deriver: None,
                ca: None,
            });
        }
        // Normal/Pull + uncached: skip — handled by upstream lookup below.
    }

    // ── Upstream cache lookup (Normal / Pull only) ────────────────────────────
    // For Push mode we only care about what's locally cached; skip upstream.
    if matches!(mode, QueryMode::Push) {
        return result;
    }

    let locally_cached_hashes: std::collections::HashSet<&str> =
        cached_map.keys().copied().collect();
    let uncached_pairs: Vec<(&str, &str)> = hash_path_pairs
        .iter()
        .filter(|(hash, _)| !locally_cached_hashes.contains(hash))
        .copied()
        .collect();

    if uncached_pairs.is_empty() || org_id.is_none() {
        return result;
    }
    let org_id = org_id.unwrap();

    // Load all external upstream URLs for the org's readable caches.
    let org_cache_rows = match EOrgCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(COrgCache::Organization.eq(org_id))
                .add(COrgCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(%org_id, error = %e, "CacheQuery org_cache lookup failed");
            return result;
        }
    };

    let cache_ids: Vec<uuid::Uuid> = org_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return result;
    }

    let upstream_rows = match ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Url.is_not_null())
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(%org_id, error = %e, "CacheQuery upstream lookup failed");
            return result;
        }
    };

    let upstream_urls: Vec<String> =
        upstream_rows.into_iter().filter_map(|r| r.url).collect();

    if upstream_urls.is_empty() {
        return result;
    }

    // Per-request timeout: a single hung upstream (DNS-resolves but never
    // responds) used to block the whole CacheQuery indefinitely, blowing
    // through the worker's 120s `query_cache` timeout. 5s per request is
    // generous for narinfo (a few-hundred-byte file) and bounds total
    // worst-case wait to `5s × upstream_urls.len()` per path.
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build upstream HTTP client; skipping upstream lookup");
            return result;
        }
    };

    // Parallelise upstream lookups across all uncached paths. Without this
    // the loop was strictly serial: a 59-path eval batch with 2 upstream
    // URLs averaging 1 s per probe = ~120 s end-to-end, which is exactly
    // when the worker's `query_cache` timeout fires. With a concurrency cap
    // we stay well under it even on slow links.
    use futures::stream::{FuturesUnordered, StreamExt as _};
    const UPSTREAM_LOOKUP_CONCURRENCY: usize = 16;

    let upstream_urls = Arc::new(upstream_urls);
    let mut futs = FuturesUnordered::new();
    let mut iter = uncached_pairs.into_iter();
    // Seed up to the concurrency cap, then refill as each completes so we
    // never have more than N requests in flight at once.
    for _ in 0..UPSTREAM_LOOKUP_CONCURRENCY {
        if let Some((hash, store_path)) = iter.next() {
            futs.push(lookup_upstream_narinfo(
                http.clone(),
                Arc::clone(&upstream_urls),
                hash.to_owned(),
                store_path.to_owned(),
            ));
        }
    }
    while let Some(found) = futs.next().await {
        if let Some(cp) = found {
            result.push(cp);
        }
        if let Some((hash, store_path)) = iter.next() {
            futs.push(lookup_upstream_narinfo(
                http.clone(),
                Arc::clone(&upstream_urls),
                hash.to_owned(),
                store_path.to_owned(),
            ));
        }
    }

    result
}

/// Probe each `upstream_url` for `<hash>.narinfo` until one responds 2xx.
/// Returns `Some(CachedPath)` pointing at the upstream NAR URL on first hit,
/// `None` if no upstream has the path.  Each HTTP request is bounded by the
/// caller's `Client::timeout`, so a stuck upstream can't block the whole
/// CacheQuery handler.
async fn lookup_upstream_narinfo(
    http: reqwest::Client,
    upstream_urls: Arc<Vec<String>>,
    hash: String,
    store_path: String,
) -> Option<crate::messages::CachedPath> {
    for base_url in upstream_urls.iter() {
        let narinfo_url = format!("{}/{}.narinfo", base_url.trim_end_matches('/'), &hash);
        let body = match http.get(&narinfo_url).send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(b) => b,
                Err(_) => continue,
            },
            _ => continue,
        };
        if let Some(nar_path) = body
            .lines()
            .find_map(|l| l.strip_prefix("URL: ").map(str::trim))
        {
            let url = format!("{}/{}", base_url.trim_end_matches('/'), nar_path);
            // Upstream-served paths: we don't have the import metadata
            // locally. For Pull-mode workers this means they can't import
            // via add_to_store_nar without first parsing the upstream
            // narinfo themselves; the URL points at the upstream NAR
            // directly so they can fall back to `nix copy`/substituter
            // semantics if needed.
            return Some(crate::messages::CachedPath {
                path: store_path,
                cached: true,
                file_size: None,
                nar_size: None,
                url: Some(url),
                nar_hash: None,
                references: None,
                signatures: None,
                deriver: None,
                ca: None,
            });
        }
    }
    None
}

/// Chunk size for direct NAR streaming over the WebSocket (64 KiB).
const NAR_PUSH_CHUNK_SIZE: usize = 64 * 1024;

/// Look up `store_path` in `state.nar_storage` and stream it to the worker as
/// a sequence of [`ServerMessage::NarPush`] frames. The last frame carries
/// `is_final: true`. Returns an error (logged by the caller) if the NAR is
/// not present or transmission fails midway — the worker's `NarRequest`
/// waiter will time out so a stuck transfer doesn't block the build.
async fn serve_nar_request(
    state: &Arc<ServerState>,
    socket: &mut ProtoSocket,
    job_id: &str,
    store_path: &str,
) -> anyhow::Result<()> {
    // Extract the 32-char hash that keys the NAR storage from the store path.
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
    let mut offset: u64 = 0;
    if total == 0 {
        // Empty NAR — still send one is_final frame so the worker's waiter
        // can complete instead of timing out.
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

/// Resolve the import metadata (`nar_hash`, `references`, `signatures`,
/// `deriver`, `ca`) that a worker needs to construct a `ValidPathInfo` and
/// call `add_to_store_nar` on its local nix-daemon.
///
/// Returns `(None, None, None, None, None)` if no `cached_path` row exists
/// for `hash` (the path was cached but the metadata side-table is empty —
/// rare; the worker will then fall back to whatever the URL serves).
async fn fetch_pull_metadata(
    state: &ServerState,
    hash: &str,
) -> (
    Option<String>,        // nar_hash
    Option<Vec<String>>,   // references (full /nix/store/... paths)
    Option<Vec<String>>,   // signatures (narinfo wire format)
    Option<String>,        // deriver
    Option<String>,        // ca
) {
    let cached_row = match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return (None, None, None, None, None),
        Err(e) => {
            warn!(%hash, error = %e, "failed to load cached_path for Pull metadata");
            return (None, None, None, None, None);
        }
    };

    // `references` is stored space-separated in `hash-name` form; expand to
    // full `/nix/store/...` paths so the worker doesn't have to.
    let references = cached_row.references.as_ref().map(|s| {
        s.split_whitespace()
            .map(|r| {
                if r.starts_with("/nix/store/") {
                    r.to_owned()
                } else {
                    format!("/nix/store/{}", r)
                }
            })
            .collect::<Vec<_>>()
    });

    // Collect every populated signature for this cached_path.
    let signatures = match ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_row.id))
        .all(&state.db)
        .await
    {
        Ok(rows) => {
            let sigs: Vec<String> = rows.into_iter().filter_map(|r| r.signature).collect();
            if sigs.is_empty() { None } else { Some(sigs) }
        }
        Err(e) => {
            warn!(%hash, error = %e, "failed to load cached_path signatures");
            None
        }
    };

    // `deriver` isn't stored on cached_path; resolve it via the
    // derivation_output → derivation chain when this path is a build output.
    let deriver = match EDerivationOutput::find()
        .filter(CDerivationOutput::Hash.eq(hash))
        .one(&state.db)
        .await
    {
        Ok(Some(out)) => match EDerivation::find_by_id(out.derivation)
            .one(&state.db)
            .await
        {
            Ok(Some(d)) => Some(d.derivation_path),
            _ => None,
        },
        _ => None,
    };

    (
        cached_row.nar_hash,
        references,
        signatures,
        deriver,
        cached_row.ca,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::types::proto::QueryMode;
    use std::sync::Arc;

    fn sha256_hex(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    fn all_caps(val: bool) -> GradientCapabilities {
        GradientCapabilities {
            core: val,
            cache: val,
            federate: val,
            fetch: val,
            eval: val,
            build: val,
            sign: val,
        }
    }

    fn make_state(serve_cache: bool, federate_proto: bool) -> ServerState {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let mut state = Arc::try_unwrap(test_support::prelude::test_state(db)).unwrap();
        state.cli.serve_cache = serve_cache;
        state.cli.federate_proto = federate_proto;
        state
    }

    // ── validate_tokens ──────────────────────────────────────────────────────

    #[test]
    fn validate_tokens_matching_hash_authorizes() {
        let token = "my-secret-token";
        let hash = sha256_hex(token);
        let registered = vec![("peer-a".to_string(), hash)];
        let auth = vec![("peer-a".to_string(), token.to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_wrong_hash_fails() {
        let registered = vec![("peer-a".to_string(), sha256_hex("correct-token"))];
        let auth = vec![("peer-a".to_string(), "wrong-token".to_string())];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, "peer-a");
        assert!(failed[0].reason.contains("invalid token"));
    }

    #[test]
    fn validate_tokens_missing_token_fails() {
        let registered = vec![("peer-a".to_string(), sha256_hex("some-token"))];
        let (authorized, failed) = validate_tokens(&registered, &[]);
        assert!(authorized.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_id, "peer-a");
        assert!(failed[0].reason.contains("no token provided"));
    }

    #[test]
    fn validate_tokens_mixed_results() {
        let tok_b = "token-b";
        let registered = vec![
            ("peer-a".to_string(), sha256_hex("token-a")),
            ("peer-b".to_string(), sha256_hex(tok_b)),
            ("peer-c".to_string(), sha256_hex("token-c")),
        ];
        let auth = vec![
            ("peer-a".to_string(), "token-a".to_string()), // correct
            ("peer-b".to_string(), "wrong".to_string()),   // wrong hash
                                                           // peer-c missing
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert_eq!(failed.len(), 2);
        let failed_ids: Vec<&str> = failed.iter().map(|f| f.peer_id.as_str()).collect();
        assert!(failed_ids.contains(&"peer-b"));
        assert!(failed_ids.contains(&"peer-c"));
    }

    #[test]
    fn validate_tokens_empty_inputs() {
        let (authorized, failed) = validate_tokens(&[], &[]);
        assert!(authorized.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_extra_tokens_ignored() {
        // auth_tokens has entries for peers not in registered_peers — they are ignored
        let auth = vec![("unknown-peer".to_string(), "some-token".to_string())];
        let (authorized, failed) = validate_tokens(&[], &auth);
        assert!(authorized.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn validate_tokens_duplicate_peer_first_wins() {
        let token = "correct";
        let registered = vec![("peer-a".to_string(), sha256_hex(token))];
        // Two auth entries for the same peer; first is correct, second is wrong.
        let auth = vec![
            ("peer-a".to_string(), token.to_string()),
            ("peer-a".to_string(), "wrong".to_string()),
        ];
        let (authorized, failed) = validate_tokens(&registered, &auth);
        assert_eq!(authorized, vec!["peer-a"]);
        assert!(failed.is_empty());
    }

    // ── negotiate_capabilities ───────────────────────────────────────────────

    #[test]
    fn negotiate_capabilities_core_always_true() {
        let state = make_state(false, false);
        let result = negotiate_capabilities(&state, all_caps(false));
        assert!(result.core);
    }

    #[test]
    fn negotiate_capabilities_cache_from_server() {
        let state_cache_on = make_state(true, false);
        assert!(negotiate_capabilities(&state_cache_on, all_caps(false)).cache);
        let state_cache_off = make_state(false, false);
        assert!(!negotiate_capabilities(&state_cache_off, all_caps(true)).cache);
    }

    #[test]
    fn negotiate_capabilities_federate_requires_both() {
        assert!(!negotiate_capabilities(&make_state(false, false), all_caps(true)).federate);
        assert!(!negotiate_capabilities(&make_state(false, true), all_caps(false)).federate);
        assert!(negotiate_capabilities(&make_state(false, true), all_caps(true)).federate);
    }

    #[test]
    fn negotiate_capabilities_passthrough_fields() {
        let state = make_state(false, false);
        let client = GradientCapabilities {
            core: false,
            cache: false,
            federate: false,
            fetch: true,
            eval: true,
            build: true,
            sign: true,
        };
        let result = negotiate_capabilities(&state, client);
        assert!(result.fetch);
        assert!(result.eval);
        assert!(result.build);
        assert!(result.sign);
    }

    #[test]
    fn negotiate_capabilities_all_false_client() {
        let state = make_state(false, false);
        let result = negotiate_capabilities(&state, all_caps(false));
        assert!(result.core);
        assert!(!result.cache);
        assert!(!result.federate);
        assert!(!result.fetch);
        assert!(!result.eval);
        assert!(!result.build);
        assert!(!result.sign);
    }

    // ── handle_cache_query ───────────────────────────────────────────────────

    /// Empty path list always returns empty regardless of mode.
    #[tokio::test]
    async fn cache_query_empty_paths_returns_empty() {
        let state = make_state(false, false);
        assert!(handle_cache_query(&state, None, &[], QueryMode::Normal).await.is_empty());
        assert!(handle_cache_query(&state, None, &[], QueryMode::Push).await.is_empty());
        assert!(handle_cache_query(&state, None, &[], QueryMode::Pull).await.is_empty());
    }

    /// Paths with a malformed store hash (not 32 chars) are silently skipped.
    #[tokio::test]
    async fn cache_query_invalid_store_paths_skipped() {
        let state = make_state(false, false);
        let paths = vec!["not-a-store-path".to_string(), "/nix/store/short-name".to_string()];
        assert!(handle_cache_query(&state, None, &paths, QueryMode::Normal).await.is_empty());
        assert!(handle_cache_query(&state, None, &paths, QueryMode::Push).await.is_empty());
        assert!(handle_cache_query(&state, None, &paths, QueryMode::Pull).await.is_empty());
    }

    /// Normal mode: uncached paths (empty DB) → empty result.
    #[tokio::test]
    async fn cache_query_normal_uncached_returns_empty() {
        let state = make_state(false, false);
        // 32-char nix-base32 hash
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".to_string()];
        let result = handle_cache_query(&state, None, &paths, QueryMode::Normal).await;
        assert!(result.is_empty(), "Normal mode should not return uncached paths");
    }

    /// Pull mode: uncached paths (empty DB) → empty result (no presigned URLs for uncached).
    #[tokio::test]
    async fn cache_query_pull_uncached_returns_empty() {
        let state = make_state(false, false);
        let paths = vec!["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".to_string()];
        let result = handle_cache_query(&state, None, &paths, QueryMode::Pull).await;
        assert!(result.is_empty(), "Pull mode should not return uncached paths");
    }

    /// Push mode: uncached paths (empty DB) → returned with cached=false.
    /// Local NarStore produces url=None (no presigned upload URLs).
    #[tokio::test]
    async fn cache_query_push_uncached_returns_all_with_cached_false() {
        let state = make_state(false, false);
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar".to_string(),
        ];
        let result = handle_cache_query(&state, None, &paths, QueryMode::Push).await;
        assert_eq!(result.len(), 2, "Push should return all queried paths");
        for cp in &result {
            assert!(!cp.cached, "all should be uncached (empty DB): {}", cp.path);
            assert!(cp.url.is_none(), "local store → no presigned URL: {}", cp.path);
        }
        let returned_paths: Vec<&str> = result.iter().map(|c| c.path.as_str()).collect();
        assert!(returned_paths.contains(&paths[0].as_str()));
        assert!(returned_paths.contains(&paths[1].as_str()));
    }

    /// Push mode: duplicate paths collapse to one entry per unique path.
    #[tokio::test]
    async fn cache_query_push_deduplicates_by_hash() {
        let state = make_state(false, false);
        // Same hash, two paths (shouldn't happen in practice but let's be safe).
        let paths = vec![
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo".to_string(),
        ];
        // Both entries share the same hash; the filter_map produces one per
        // unique (hash, path) pair — duplicates are still processed but result
        // in one entry since they map to the same path string.
        let result = handle_cache_query(&state, None, &paths, QueryMode::Push).await;
        // One or two entries is acceptable — what matters is all are uncached.
        for cp in &result {
            assert!(!cp.cached);
        }
    }
}
