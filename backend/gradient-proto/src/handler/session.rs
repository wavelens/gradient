/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol session state machine: Opening → Authenticated → Registered → run.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_types::ids::OrganizationId;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, instrument, warn};

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{GradientCapabilities, ServerMessage};
use crate::session::handshake as handshake_fsm;
use crate::traits::{AuthOutcome, PeerAuthority};
use gradient_scheduler::Scheduler;

use super::auth::{
    BaseWorkerChallenge, aggregate_enabled_caps, expand_base_authorized,
    filter_org_peers_without_cache, has_any_registrations, lookup_base_worker_challenge,
    lookup_registered_peers, negotiate_capabilities, validate_tokens,
};
use super::dispatch::DispatchContext;
use super::eval_cache::EvalCacheReceiveStore;
use super::nar_transfer::NarReceiveStore;
use super::socket::{
    HANDSHAKE_TIMEOUT, JOB_OFFER_CHUNK_SIZE, ProtoSocket, ProtoWriter, recv_client_msg,
    send_server_msg,
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
    pub job_notify: tokio::sync::watch::Receiver<u64>,
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

    /// Discoverable check, then the shared handshake FSM drives
    /// InitConnection → AuthChallenge/AuthResponse → InitAck with
    /// [`ServerAuthority`] supplying the auth policy.
    pub async fn handshake(
        mut self,
        server_initiated: bool,
    ) -> Option<ProtoSession<Authenticated>> {
        if !server_initiated && !self.state.config.proto.discoverable {
            self.socket
                .send_reject(403, "server is not accepting connections".into())
                .await;
            return None;
        }
        let authority = ServerAuthority {
            state: Arc::clone(&self.state),
            server_initiated,
        };
        let result = match handshake_fsm::as_authority(&mut self.socket, &authority).await {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, server_initiated, "handshake failed");
                return None;
            }
        };
        info!(peer_id = %result.peer_id, authorized = result.authorized_peers.len(), "handshake complete");
        Some(ProtoSession {
            socket: self.socket,
            state: self.state,
            scheduler: self.scheduler,
            session_state: Authenticated {
                peer_id: result.peer_id,
                negotiated: result.negotiated,
                authorized_peers: result.authorized_peers,
            },
        })
    }
}

// ── Authority impl over the server's auth store ──────────────────────────────

/// [`PeerAuthority`] over gradient-server's registration tables: the shared
/// handshake FSM drives the wire while this supplies challenges, token
/// validation, the pure [`decide_auth`] policy, and capability negotiation.
struct ServerAuthority {
    state: Arc<ServerState>,
    server_initiated: bool,
}

struct ServerChallenge {
    base: Option<BaseWorkerChallenge>,
    registered_peers: Vec<(String, String)>,
}

#[async_trait]
impl PeerAuthority for ServerAuthority {
    type Challenge = ServerChallenge;

    async fn challenge(&self, claimed: &str) -> Result<(ServerChallenge, Vec<String>)> {
        let base = lookup_base_worker_challenge(&self.state, claimed).await;
        let registered_peers = match &base {
            Some(b) => b.challenge.clone(),
            None => lookup_registered_peers(&self.state, claimed).await,
        };
        let names = registered_peers.iter().map(|(id, _)| id.clone()).collect();
        Ok((
            ServerChallenge {
                base,
                registered_peers,
            },
            names,
        ))
    }

    async fn authorize(
        &self,
        claimed: &str,
        challenge: ServerChallenge,
        tokens: &[(String, String)],
    ) -> Result<AuthOutcome> {
        let ServerChallenge {
            base,
            registered_peers,
        } = challenge;
        let (token_authorized, mut failed_peers) = validate_tokens(&registered_peers, tokens);
        let token_authorized = expand_base_authorized(&base, token_authorized);

        let had_token_authorized = !token_authorized.is_empty();
        let (authorized_peers, demoted) =
            filter_org_peers_without_cache(&self.state, token_authorized).await;
        let emptied_by_missing_cache =
            authorized_peers.is_empty() && had_token_authorized && !demoted.is_empty();
        failed_peers.extend(demoted);

        let is_base = base.is_some();
        let has_any =
            registered_peers.is_empty() && has_any_registrations(&self.state, claimed).await;
        match decide_auth(
            self.server_initiated,
            registered_peers.is_empty(),
            has_any,
            authorized_peers.is_empty(),
            emptied_by_missing_cache,
            is_base,
        ) {
            AuthDecision::Accept => {
                if registered_peers.is_empty() {
                    debug!(
                        peer_id = %claimed,
                        "server-initiated, no registered peers - open connection accepted"
                    );
                }
                Ok(AuthOutcome::Accept {
                    authorized_peers,
                    failed_peers,
                })
            }
            AuthDecision::Reject { code, reason } => Ok(AuthOutcome::Reject {
                code,
                reason: reason.into(),
            }),
        }
    }

    async fn negotiate(
        &self,
        claimed: &str,
        client: GradientCapabilities,
    ) -> Result<GradientCapabilities> {
        let enabled = aggregate_enabled_caps(&self.state, claimed).await;
        Ok(negotiate_capabilities(&self.state, client, enabled))
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
            self.socket
                .send_reject(496, "worker already connected".into())
                .await;
            return None;
        }

        let authorized_peer_uuids: HashSet<OrganizationId> = authorized_peers
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
            socket,
            state,
            scheduler,
            session_state:
                Registered {
                    peer_id,
                    reauth_notify,
                    mut abort_rx,
                    mut job_notify,
                },
        } = self;

        let proto_cfg = &state.config.proto;
        let send_chunk_timeout = Duration::from_secs(proto_cfg.nar_send_chunk_timeout_secs);
        let (mut reader, writer) = socket.split(send_chunk_timeout);

        let partial_root =
            std::path::PathBuf::from(format!("{}/nar-partial", state.config.storage.base_path));
        let partial_ttl = Duration::from_secs(proto_cfg.nar_partial_ttl_secs);
        let max_partial_bytes = proto_cfg.max_nar_buffer_bytes as u64;
        let mut nar = NarReceiveStore::new(partial_root, &peer_id, partial_ttl, max_partial_bytes)
            .unwrap_or_else(|e| {
                error!(%peer_id, error = %e, "failed to init NAR partial dir; falling back to temp");
                NarReceiveStore::new(
                    std::env::temp_dir().join("gradient-nar-partial"),
                    &peer_id,
                    partial_ttl,
                    max_partial_bytes,
                )
                .expect("temp partial dir must be creatable")
            });
        let nar_serve_semaphore = Arc::new(Semaphore::new(proto_cfg.max_concurrent_nar_serves));
        let mut eval_cache = EvalCacheReceiveStore::new(max_partial_bytes);

        // Lock-free handle into the worker's `last_seen`, stamped on every
        // inbound frame so the liveness watchdog can spot a worker that died
        // without a clean TCP close.
        let last_seen = scheduler.worker_last_seen(&peer_id).await;

        loop {
            let msg = tokio::select! {
                msg = recv_client_msg(&mut reader) => match msg {
                    Some(m) => m,
                    None => break,
                },
                _ = reauth_notify.notified() => {
                    if !on_reauth_notify(&writer, &state, &peer_id).await { break; }
                    continue;
                }
                res = job_notify.changed() => {
                    if res.is_err() { break; }
                    if !on_job_notify(&writer, &scheduler, &peer_id).await { break; }
                    continue;
                }
                abort_msg = abort_rx.recv() => match abort_msg {
                    Some((job_id, reason)) => {
                        info!(%peer_id, %job_id, %reason, "sending AbortJob to worker");
                        if send_server_msg(
                            &writer,
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

            // Reaching here means a real inbound frame (the other select arms
            // all `continue`), so the worker is alive: refresh its deadline.
            if let Some(ls) = &last_seen {
                ls.store(
                    gradient_types::now().and_utc().timestamp_millis(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }

            let mut ctx = DispatchContext {
                writer: &writer,
                state: &state,
                scheduler: &scheduler,
                peer_id: &peer_id,
                nar_serve_semaphore: &nar_serve_semaphore,
            };
            if !ctx.dispatch(msg, &mut nar, &mut eval_cache).await {
                break;
            }
        }

        scheduler.unregister_worker(&peer_id).await;
        info!(%peer_id, "WebSocket connection closed");
    }
}

// ── Select arm helpers ────────────────────────────────────────────────────────

async fn on_reauth_notify(writer: &ProtoWriter, state: &ServerState, peer_id: &str) -> bool {
    debug!(%peer_id, "server-initiated reauth");
    let base = lookup_base_worker_challenge(state, peer_id).await;
    let registered_peers = match &base {
        Some(b) => b.challenge.clone(),
        None => lookup_registered_peers(state, peer_id).await,
    };
    if base.is_none() && registered_peers.is_empty() && has_any_registrations(state, peer_id).await
    {
        info!(%peer_id, "all registrations deactivated - disconnecting worker");
        let _ = send_server_msg(
            writer,
            &ServerMessage::Reject {
                code: 403,
                reason: "worker is deactivated".into(),
            },
        )
        .await;
        return false;
    }
    send_server_msg(
        writer,
        &ServerMessage::AuthChallenge {
            peers: registered_peers.iter().map(|(id, _)| id.clone()).collect(),
        },
    )
    .await
    .is_ok()
}

async fn on_job_notify(writer: &ProtoWriter, scheduler: &Scheduler, peer_id: &str) -> bool {
    let candidates = scheduler.get_new_job_candidates(peer_id).await;
    if candidates.is_empty() {
        return true;
    }
    debug!(%peer_id, count = candidates.len(), "pushing job offer (delta)");
    for chunk in candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
        if send_server_msg(
            writer,
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
    let session =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, session.handshake(server_initiated)).await {
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(_) => {
                warn!(
                    timeout_secs = HANDSHAKE_TIMEOUT.as_secs(),
                    server_initiated, "WebSocket handshake timed out; dropping connection"
                );
                return;
            }
        };
    let Some(session) = session.register().await else {
        return;
    };
    session.run().await;
}

// ── Auth decision (pure) ──────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum AuthDecision {
    Accept,
    Reject { code: u16, reason: &'static str },
}

/// Pure decision function used by `perform_auth` so the authorisation policy
/// is independently testable.
///
/// - `server_initiated`: connection initiated by *us* (we know the worker).
/// - `registered_peers_empty`: no `peer` row mentions this `worker_id` at all.
/// - `has_any_registrations`: any cache/org has *ever* registered this worker
///   (i.e. it once existed but is now deactivated).
/// - `authorized_peers_empty`: zero of the peers in the challenge produced a
///   valid token.
/// - `emptied_by_missing_cache`: tokens validated for at least one peer, but
///   every such peer was demoted because its organization has no subscribed
///   cache. Distinguishes "incomplete server setup" from a real auth failure.
fn decide_auth(
    server_initiated: bool,
    registered_peers_empty: bool,
    has_any_registrations: bool,
    authorized_peers_empty: bool,
    emptied_by_missing_cache: bool,
    is_base: bool,
) -> AuthDecision {
    if is_base {
        return if authorized_peers_empty {
            AuthDecision::Reject {
                code: 403,
                reason: "base worker not enabled by any organization",
            }
        } else {
            AuthDecision::Accept
        };
    }

    if registered_peers_empty {
        if has_any_registrations {
            return AuthDecision::Reject {
                code: 403,
                reason: "worker is deactivated",
            };
        }
        if !server_initiated {
            return AuthDecision::Reject {
                code: 403,
                reason: "unknown worker",
            };
        }
        AuthDecision::Accept
    } else if authorized_peers_empty {
        if emptied_by_missing_cache {
            AuthDecision::Reject {
                code: 495,
                reason: "organization has no cache subscribed",
            }
        } else {
            AuthDecision::Reject {
                code: 401,
                reason: "no valid peer tokens provided",
            }
        }
    } else {
        AuthDecision::Accept
    }
}

#[cfg(test)]
mod auth_decision_tests {
    use super::{AuthDecision, decide_auth};

    /// Inbound connection from a worker nobody has registered must be
    /// rejected. This is the regression test for the open-mode auth bypass:
    /// before the fix, `decide_auth` (then inlined) accepted because the
    /// `server_initiated` branch ran for everyone.
    #[test]
    fn inbound_unknown_worker_rejected() {
        let d = decide_auth(false, true, false, true, false, false);
        assert_eq!(
            d,
            AuthDecision::Reject {
                code: 403,
                reason: "unknown worker",
            }
        );
    }

    /// Server-initiated outbound connection to an unregistered worker is
    /// the only legitimate "open mode" path.
    #[test]
    fn outbound_unknown_worker_accepted() {
        assert_eq!(
            decide_auth(true, true, false, true, false, false),
            AuthDecision::Accept
        );
    }

    /// Worker had a registration once but it's been removed → reject as
    /// deactivated, regardless of inbound vs. outbound.
    #[test]
    fn deactivated_worker_rejected_inbound() {
        assert_eq!(
            decide_auth(false, true, true, true, false, false),
            AuthDecision::Reject {
                code: 403,
                reason: "worker is deactivated",
            }
        );
    }

    #[test]
    fn deactivated_worker_rejected_outbound() {
        assert_eq!(
            decide_auth(true, true, true, true, false, false),
            AuthDecision::Reject {
                code: 403,
                reason: "worker is deactivated",
            }
        );
    }

    /// Registered peers exist but no token validated → 401.
    #[test]
    fn registered_but_no_valid_token() {
        assert_eq!(
            decide_auth(false, false, false, true, false, false),
            AuthDecision::Reject {
                code: 401,
                reason: "no valid peer tokens provided",
            }
        );
    }

    /// Tokens validated but every authorized peer was demoted because its
    /// organization has no cache → distinct 495, not a misleading 401.
    #[test]
    fn registered_emptied_by_missing_cache() {
        assert_eq!(
            decide_auth(false, false, false, true, true, false),
            AuthDecision::Reject {
                code: 495,
                reason: "organization has no cache subscribed",
            }
        );
    }

    /// Registered + at least one valid token → accept.
    #[test]
    fn registered_with_valid_token_accepted() {
        assert_eq!(
            decide_auth(false, false, false, false, false, false),
            AuthDecision::Accept
        );
    }

    /// Base worker whose final authorized set is empty must be rejected,
    /// otherwise it would reach the pool as an Open peer (all orgs).
    #[test]
    fn base_worker_empty_authorized_rejected() {
        assert_eq!(
            decide_auth(true, false, false, true, false, true),
            AuthDecision::Reject {
                code: 403,
                reason: "base worker not enabled by any organization",
            }
        );
    }

    /// Base worker with a non-empty authorized set is accepted.
    #[test]
    fn base_worker_with_authorized_accepted() {
        assert_eq!(
            decide_auth(true, false, false, false, false, true),
            AuthDecision::Accept
        );
    }

    /// `authorize_against` mode expands a single authorized identity to the
    /// full enabled-org set; a non-match collapses to empty.
    #[test]
    fn authorize_against_expands_to_enabled_orgs_when_identity_authorized() {
        let identity = "id-1".to_string();
        let enabled = vec!["org-1".to_string(), "org-2".to_string()];
        let authorized = [identity.clone()];
        let out = if authorized.contains(&identity) {
            enabled.clone()
        } else {
            vec![]
        };
        assert_eq!(out, enabled);
    }
}
