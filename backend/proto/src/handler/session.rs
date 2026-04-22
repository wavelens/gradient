/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol session state machine: Opening → Authenticated → Registered → run.

use std::collections::HashSet;
use std::sync::Arc;

use gradient_core::types::*;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use crate::messages::{
    ClientMessage, FailedPeer, GradientCapabilities, PROTO_VERSION, ServerMessage,
};
use scheduler::Scheduler;

use super::auth::{
    aggregate_enabled_caps, filter_org_peers_without_cache, has_any_registrations,
    lookup_registered_peers, negotiate_capabilities, validate_tokens,
};
use super::dispatch::DispatchContext;
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoSocket, recv_client_msg, send_error, send_reject, send_server_msg,
};

// ── Session state markers ─────────────────────────────────────────────────────

pub(super) struct Opening;

pub(super) struct Authenticated {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
}

pub(super) struct Registered {
    pub peer_id: String,
    pub reauth_notify: Arc<tokio::sync::Notify>,
    pub abort_rx: tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    pub job_notify: Arc<tokio::sync::Notify>,
}

// ── Protocol session ──────────────────────────────────────────────────────────

pub(super) struct ProtoSession<S> {
    pub socket: ProtoSocket,
    pub state: Arc<ServerState>,
    pub scheduler: Arc<Scheduler>,
    pub session_state: S,
}

// ── Opening → Authenticated ───────────────────────────────────────────────────

impl ProtoSession<Opening> {
    pub fn new(socket: ProtoSocket, state: Arc<ServerState>, scheduler: Arc<Scheduler>) -> Self {
        Self {
            socket,
            state,
            scheduler,
            session_state: Opening,
        }
    }

    /// Discoverable check → InitConnection → auth challenge/response → InitAck.
    pub async fn handshake(
        mut self,
        server_initiated: bool,
    ) -> Option<ProtoSession<Authenticated>> {
        if !server_initiated && !self.state.cli.discoverable {
            send_reject(
                &mut self.socket,
                403,
                "server is not accepting connections".into(),
            )
            .await;
            return None;
        }
        let (peer_id, client_capabilities) = self.recv_init_connection().await?;
        let (authorized_peers, failed_peers) = self.perform_auth(&peer_id).await?;
        let enabled_caps = aggregate_enabled_caps(&self.state, &peer_id).await;
        let negotiated = negotiate_capabilities(&self.state, client_capabilities, enabled_caps);
        self.send_init_ack(&negotiated, &authorized_peers, &failed_peers)
            .await
            .ok()?;
        info!(%peer_id, authorized = authorized_peers.len(), "handshake complete");
        Some(ProtoSession {
            socket: self.socket,
            state: self.state,
            scheduler: self.scheduler,
            session_state: Authenticated {
                peer_id,
                negotiated,
                authorized_peers,
            },
        })
    }

    async fn recv_init_connection(&mut self) -> Option<(String, GradientCapabilities)> {
        let msg = recv_client_msg(&mut self.socket).await?;
        match msg {
            ClientMessage::InitConnection {
                version,
                capabilities,
                id,
            } => {
                debug!(version, ?capabilities, %id, "InitConnection received");
                if version != PROTO_VERSION {
                    send_reject(
                        &mut self.socket,
                        400,
                        format!("unsupported protocol version {version}"),
                    )
                    .await;
                    return None;
                }
                Some((id, capabilities))
            }
            _ => {
                send_error(&mut self.socket, 400, "expected InitConnection".into()).await;
                None
            }
        }
    }

    async fn perform_auth(&mut self, peer_id: &str) -> Option<(Vec<String>, Vec<FailedPeer>)> {
        let registered_peers = lookup_registered_peers(&self.state, peer_id).await;
        send_server_msg(
            &mut self.socket,
            &ServerMessage::AuthChallenge {
                peers: registered_peers.iter().map(|(id, _)| id.clone()).collect(),
            },
        )
        .await
        .ok()?;

        let tokens = match recv_client_msg(&mut self.socket).await {
            Some(ClientMessage::AuthResponse { tokens }) => tokens,
            Some(_) => {
                send_error(&mut self.socket, 400, "expected AuthResponse".into()).await;
                return None;
            }
            None => return None,
        };

        let (authorized_peers, mut failed_peers) = validate_tokens(&registered_peers, &tokens);
        let (authorized_peers, demoted) =
            filter_org_peers_without_cache(&self.state, authorized_peers).await;
        failed_peers.extend(demoted);

        if registered_peers.is_empty() {
            if has_any_registrations(&self.state, peer_id).await {
                send_reject(&mut self.socket, 403, "worker is deactivated".into()).await;
                return None;
            }
            debug!(%peer_id, "no registered peers — open connection accepted");
        } else if authorized_peers.is_empty() {
            send_reject(
                &mut self.socket,
                401,
                "no valid peer tokens provided".into(),
            )
            .await;
            return None;
        }

        Some((authorized_peers, failed_peers))
    }

    async fn send_init_ack(
        &mut self,
        negotiated: &GradientCapabilities,
        authorized_peers: &[String],
        failed_peers: &[FailedPeer],
    ) -> Result<(), ()> {
        send_server_msg(
            &mut self.socket,
            &ServerMessage::InitAck {
                version: PROTO_VERSION,
                capabilities: negotiated.clone(),
                authorized_peers: authorized_peers.to_vec(),
                failed_peers: failed_peers.to_vec(),
            },
        )
        .await
    }
}

// ── Authenticated → Registered ────────────────────────────────────────────────

impl ProtoSession<Authenticated> {
    pub async fn register(mut self) -> Option<ProtoSession<Registered>> {
        let Authenticated {
            peer_id,
            negotiated,
            authorized_peers,
            ..
        } = self.session_state;

        if self.scheduler.is_worker_connected(&peer_id).await {
            warn!(%peer_id, "duplicate connection rejected (worker already connected)");
            send_reject(&mut self.socket, 496, "worker already connected".into()).await;
            return None;
        }

        let authorized_peer_uuids: HashSet<Uuid> = authorized_peers
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        let (reauth_notify, abort_rx) = self
            .scheduler
            .register_worker(&peer_id, negotiated, authorized_peer_uuids)
            .await;
        let job_notify = self.scheduler.job_notify();

        Some(ProtoSession {
            socket: self.socket,
            state: self.state,
            scheduler: self.scheduler,
            session_state: Registered {
                peer_id,
                reauth_notify,
                abort_rx,
                job_notify,
            },
        })
    }
}

// ── Registered: dispatch loop ─────────────────────────────────────────────────

impl ProtoSession<Registered> {
    pub async fn run(self) {
        let ProtoSession {
            mut socket,
            state,
            scheduler,
            session_state:
                Registered {
                    peer_id,
                    reauth_notify,
                    mut abort_rx,
                    job_notify,
                },
        } = self;

        let mut nar_buffers: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();

        loop {
            let msg = tokio::select! {
                msg = recv_client_msg(&mut socket) => match msg {
                    Some(m) => m,
                    None => break,
                },
                _ = reauth_notify.notified() => {
                    if !on_reauth_notify(&mut socket, &state, &peer_id).await { break; }
                    continue;
                }
                _ = job_notify.notified() => {
                    if !on_job_notify(&mut socket, &scheduler, &peer_id).await { break; }
                    continue;
                }
                abort_msg = abort_rx.recv() => match abort_msg {
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
                    None => break,
                },
            };

            let mut ctx = DispatchContext {
                socket: &mut socket,
                state: &state,
                scheduler: &scheduler,
                peer_id: &peer_id,
            };
            if !ctx.dispatch(msg, &mut nar_buffers).await {
                break;
            }
        }

        scheduler.unregister_worker(&peer_id).await;
        info!(%peer_id, "WebSocket connection closed");
    }
}

// ── Select arm helpers ────────────────────────────────────────────────────────

async fn on_reauth_notify(socket: &mut ProtoSocket, state: &ServerState, peer_id: &str) -> bool {
    debug!(%peer_id, "server-initiated reauth");
    let registered_peers = lookup_registered_peers(state, peer_id).await;
    if registered_peers.is_empty() && has_any_registrations(state, peer_id).await {
        info!(%peer_id, "all registrations deactivated — disconnecting worker");
        send_reject(socket, 403, "worker is deactivated".into()).await;
        return false;
    }
    send_server_msg(
        socket,
        &ServerMessage::AuthChallenge {
            peers: registered_peers.iter().map(|(id, _)| id.clone()).collect(),
        },
    )
    .await
    .is_ok()
}

async fn on_job_notify(socket: &mut ProtoSocket, scheduler: &Scheduler, peer_id: &str) -> bool {
    let candidates = scheduler.get_new_job_candidates(peer_id).await;
    if candidates.is_empty() {
        return true;
    }
    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta)");
    for chunk in candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
        if send_server_msg(
            socket,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await
        .is_err()
        {
            return false;
        }
    }
    true
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[instrument(skip_all)]
pub(crate) async fn handle_socket(
    socket: ProtoSocket,
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    server_initiated: bool,
) {
    info!(server_initiated, "WebSocket connection opened");
    let session = ProtoSession::new(socket, state, scheduler);
    let Some(session) = session.handshake(server_initiated).await else {
        return;
    };
    let Some(session) = session.register().await else {
        return;
    };
    session.run().await;
}
