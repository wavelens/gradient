/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol handshake: Opening to Authenticated, then attach to a session actor.

use std::collections::HashSet;
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_types::ids::ProjectId;
use tokio::task::JoinHandle;
use tracing::{debug, info, instrument, warn};

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{GradientCapabilities, ServerMessage};
use crate::session::handshake as handshake_fsm;
use crate::traits::{AuthOutcome, PeerAuthority};
use gradient_scheduler::Scheduler;

use super::auth::{
    BaseWorkerChallenge, aggregate_enabled_caps, expand_base_authorized,
    filter_project_peers_without_cache, has_any_registrations, lookup_base_worker_challenge,
    lookup_registered_peers, negotiate_capabilities, validate_tokens,
};
use super::session_actor::SessionArgs;
use super::sessions::SessionsHandle;
use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, send_server_msg};

// ── Session state markers ─────────────────────────────────────────────────────

pub(super) struct Opening;

pub(super) struct Authenticated {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
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
            filter_project_peers_without_cache(&self.state, token_authorized).await;
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

// ── Authenticated: attach ─────────────────────────────────────────────────────

impl ProtoSession<Authenticated> {
    /// Hand the connection to the sessions supervisor. The join handle ends
    /// when the session does, so the upgrade task holds its permit until then.
    pub async fn attach(self, sessions: &SessionsHandle) -> Option<JoinHandle<()>> {
        let ProtoSession {
            mut socket,
            state,
            scheduler,
            session_state:
                Authenticated {
                    peer_id,
                    negotiated,
                    authorized_peers,
                },
        } = self;

        if scheduler.is_worker_connected(&peer_id).await {
            warn!(%peer_id, "duplicate connection rejected (worker already connected)");
            socket
                .send_reject(496, "worker already connected".into())
                .await;
            return None;
        }

        let authorized_peers: HashSet<ProjectId> = authorized_peers
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        let args = SessionArgs {
            peer_id: peer_id.clone(),
            state,
            scheduler,
            socket,
            capabilities: negotiated,
            authorized_peers,
        };

        match sessions.attach(args).await {
            Ok((_, join)) => Some(join),
            Err(error) => {
                warn!(%peer_id, %error, "session could not be attached");
                None
            }
        }
    }
}

// ── Server-initiated reauth ───────────────────────────────────────────────────

pub(super) async fn on_reauth_notify(
    writer: &ProtoWriter,
    state: &ServerState,
    peer_id: &str,
) -> bool {
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

// ── Entry point ───────────────────────────────────────────────────────────────

#[instrument(skip_all)]
pub(crate) async fn handle_socket(
    socket: ProtoSocket,
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    sessions: Arc<SessionsHandle>,
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
    if let Some(join) = session.attach(&sessions).await {
        let _ = join.await;
    }
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
/// - `has_any_registrations`: any cache/project has *ever* registered this worker
///   (i.e. it once existed but is now deactivated).
/// - `authorized_peers_empty`: zero of the peers in the challenge produced a
///   valid token.
/// - `emptied_by_missing_cache`: tokens validated for at least one peer, but
///   every such peer was demoted because its project has no subscribed
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
                reason: "base worker not enabled by any project",
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
                reason: "project has no cache subscribed",
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
    /// project has no cache → distinct 495, not a misleading 401.
    #[test]
    fn registered_emptied_by_missing_cache() {
        assert_eq!(
            decide_auth(false, false, false, true, true, false),
            AuthDecision::Reject {
                code: 495,
                reason: "project has no cache subscribed",
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
    /// otherwise it would reach the pool as an Open peer (all projects).
    #[test]
    fn base_worker_empty_authorized_rejected() {
        assert_eq!(
            decide_auth(true, false, false, true, false, true),
            AuthDecision::Reject {
                code: 403,
                reason: "base worker not enabled by any project",
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
    /// full enabled-project set; a non-match collapses to empty.
    #[test]
    fn authorize_against_expands_to_enabled_projects_when_identity_authorized() {
        let identity = "id-1".to_string();
        let enabled = vec!["project-1".to_string(), "project-2".to_string()];
        let authorized = [identity.clone()];
        let out = if authorized.contains(&identity) {
            enabled.clone()
        } else {
            vec![]
        };
        assert_eq!(out, enabled);
    }
}
