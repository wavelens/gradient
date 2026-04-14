/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

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
    let reauth_notify = scheduler
        .register_worker(&peer_id, negotiated.clone(), authorized_peer_uuids)
        .await;
    let job_notify = scheduler.job_notify();

    // ── Dispatch loop ─────────────────────────────────────────────────────────

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
                // New jobs enqueued — push candidates to this worker.
                let candidates = scheduler.get_job_candidates(&peer_id).await;
                if !candidates.is_empty() {
                    debug!(%peer_id, count = candidates.len(), "pushing job offer");
                    if send_server_msg(
                        &mut socket,
                        &ServerMessage::JobOffer { candidates },
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
                continue;
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
                let is_final = true;
                let chunk = ServerMessage::JobListChunk {
                    candidates,
                    is_final,
                };
                if send_server_msg(&mut socket, &chunk).await.is_err() {
                    break;
                }
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
                        // TODO: record fetched input paths and upload NARs to cache.
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
                    } => {
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
                // TODO: look up each path in NarStore; respond NarPush or PresignedDownload.
            }

            ClientMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                debug!(%peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");
                // TODO: write chunk to NarStore; on is_final verify hash + import.
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
            }

            // ── Cache queries ────────────────────────────────────────────
            ClientMessage::CacheQuery { job_id, paths } => {
                debug!(%peer_id, %job_id, count = paths.len(), "CacheQuery");
                let cached = handle_cache_query(&state, &paths).await;
                debug!(%peer_id, %job_id, cached = cached.len(), "CacheStatus");
                if send_server_msg(
                    &mut socket,
                    &ServerMessage::CacheStatus {
                        job_id,
                        cached,
                    },
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
            if !flake_job.tasks.contains(&FlakeTask::FetchFlake) {
                return;
            }
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
        Job::Build(build_job) => {
            if build_job.sign.is_none() {
                return;
            }
            // Look up a signing key from the org's caches.
            // Find caches associated with this org that have signing keys.
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
                                debug!(cache_name = %cache.name, "signing key credential sent");
                                break;
                            }
                            Ok(_) => {}
                            Err(e) => warn!(error = %e, "failed to fetch cache for signing key"),
                        }
                    }
                }
                Err(e) => warn!(%org_id, error = %e, "failed to fetch org caches for signing key"),
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

/// Returns `(peer_id, token_hash)` pairs for all peers that registered this worker.
async fn lookup_registered_peers(state: &ServerState, worker_id: &str) -> Vec<(String, String)> {
    use entity::worker_registration::{Column, Entity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
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

/// Check which store paths have cached NARs in the server's cache storage.
///
/// Extracts the nix32 hash prefix from each `/nix/store/<hash>-<name>` path and
/// batch-queries the `derivation_output` table for rows with `is_cached = true`.
async fn handle_cache_query(state: &ServerState, paths: &[String]) -> Vec<String> {
    use entity::derivation_output::{Column as CDerivationOutput, Entity as EDerivationOutput};

    // Extract (hash, original_path) pairs from store paths.
    let hash_path_pairs: Vec<(&str, &str)> = paths
        .iter()
        .filter_map(|p| {
            let base = p.strip_prefix("/nix/store/").unwrap_or(p);
            let hash = base.split('-').next()?;
            if hash.len() == 32 {
                Some((hash, p.as_str()))
            } else {
                None
            }
        })
        .collect();

    if hash_path_pairs.is_empty() {
        return vec![];
    }

    let hashes: Vec<&str> = hash_path_pairs.iter().map(|(h, _)| *h).collect();

    // Batch query: find all derivation_output rows with is_cached=true for these hashes.
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
            warn!(error = %e, "CacheQuery DB lookup failed");
            return vec![];
        }
    };

    let cached_hashes: std::collections::HashSet<&str> =
        cached_rows.iter().map(|r| r.hash.as_str()).collect();

    hash_path_pairs
        .iter()
        .filter(|(hash, _)| cached_hashes.contains(hash))
        .map(|(_, path)| path.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
