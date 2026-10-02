/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_types::ids::ProjectId;
use tokio::task::JoinHandle;
use tracing::{debug, info, instrument, warn};

use anyhow::Result;
use async_trait::async_trait;

use gradient_scheduler::Scheduler;
use gradient_scheduler::connection_failures::ConnectionDirection;
use gradient_wire::messages::{GradientCapabilities, ServerMessage};
use gradient_wire::session::handshake as handshake_fsm;
use gradient_wire::session::handshake::{HandshakeResult, Rejected};
use gradient_wire::traits::{AuthOutcome, PeerAuthority};

use super::auth::{
    BaseWorkerChallenge, aggregate_enabled_caps, expand_base_authorized,
    filter_project_peers_without_cache, has_any_registrations, lookup_base_worker_challenge,
    lookup_registered_peers, negotiate_capabilities,
};
use super::dialed::DialedWorkerAuthority;
use super::failures::{record_dialed, record_unproven, rejection_reason};
use super::session_actor::SessionArgs;
use super::sessions::SessionsHandle;
use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, send_server_msg};
use crate::outbound::DialTarget;
use gradient_wire::auth::validate_tokens;

pub(crate) enum SessionOrigin {
    WorkerDialed,
    ServerDialed(DialTarget),
}

impl SessionOrigin {
    fn dialed_url(&self) -> Option<String> {
        match self {
            SessionOrigin::WorkerDialed => None,
            SessionOrigin::ServerDialed(target) => Some(target.url.clone()),
        }
    }
}

pub(super) struct Opening;

pub(super) struct Authenticated {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
    pub dialed_url: Option<String>,
}

pub(super) struct ProtoSession<S> {
    pub socket: ProtoSocket,
    pub state: Arc<ServerState>,
    pub scheduler: Arc<Scheduler>,
    pub session_state: S,
}

impl ProtoSession<Opening> {
    pub fn new(socket: ProtoSocket, state: Arc<ServerState>, scheduler: Arc<Scheduler>) -> Self {
        Self {
            socket,
            state,
            scheduler,
            session_state: Opening,
        }
    }

    pub async fn handshake(
        mut self,
        origin: &SessionOrigin,
    ) -> Option<ProtoSession<Authenticated>> {
        let outcome = match origin {
            SessionOrigin::WorkerDialed => self.accept_worker().await,
            SessionOrigin::ServerDialed(target) => self.dial_worker(target).await,
        };
        let result = match outcome {
            Ok(result) => result,
            Err(e) => {
                debug!(error = %e, "handshake failed");
                record_rejection(&self.state, &self.scheduler, origin, &e).await;
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
                dialed_url: origin.dialed_url(),
            },
        })
    }

    async fn accept_worker(&mut self) -> Result<HandshakeResult> {
        if !self.state.config.proto.discoverable {
            self.socket
                .send_reject(403, "server is not accepting connections".into())
                .await;
            anyhow::bail!("server is not accepting connections");
        }

        let authority = ServerAuthority {
            state: Arc::clone(&self.state),
        };
        handshake_fsm::as_authority(&mut self.socket, &authority).await
    }

    async fn dial_worker(&mut self, target: &DialTarget) -> Result<HandshakeResult> {
        let authority = DialedWorkerAuthority {
            state: Arc::clone(&self.state),
            url: target.url.clone(),
        };
        handshake_fsm::as_dialer(&mut self.socket, &target.credentials, &authority).await
    }
}

async fn record_rejection(
    state: &ServerState,
    scheduler: &Scheduler,
    origin: &SessionOrigin,
    error: &anyhow::Error,
) {
    let failures = &scheduler.connection_failures;
    match (origin, error.downcast_ref::<Rejected>()) {
        (SessionOrigin::WorkerDialed, Some(rejected)) => {
            record_unproven(state, failures, rejected).await;
        }
        (SessionOrigin::WorkerDialed, None) => {}
        (SessionOrigin::ServerDialed(target), Some(rejected)) => {
            record_dialed(failures, &target.credentials.worker_id, rejected);
        }
        (SessionOrigin::ServerDialed(target), None) => failures.record(
            &target.credentials.worker_id,
            ConnectionDirection::Outbound,
            false,
            format!("handshake failed: {error:#}"),
        ),
    }
}

struct ServerAuthority {
    state: Arc<ServerState>,
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
            registered_peers.is_empty(),
            has_any,
            authorized_peers.is_empty(),
            emptied_by_missing_cache,
            is_base,
        ) {
            AuthDecision::Accept => Ok(AuthOutcome::Accept {
                authorized_peers,
                failed_peers,
            }),
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

impl ProtoSession<Authenticated> {
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
                    dialed_url,
                },
        } = self;

        if scheduler.is_worker_connected(&peer_id).await {
            warn!(%peer_id, "duplicate connection rejected (worker already connected)");
            let direction = match dialed_url {
                Some(_) => ConnectionDirection::Outbound,
                None => ConnectionDirection::Inbound,
            };
            scheduler.connection_failures.record(
                &peer_id,
                direction,
                false,
                rejection_reason(496, "worker already connected"),
            );
            socket
                .send_reject(496, "worker already connected".into())
                .await;
            return None;
        }

        let failures = Arc::clone(&scheduler.connection_failures);
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
            dialed_url,
        };

        match sessions.attach(args).await {
            Ok((_, join)) => {
                failures.clear(&peer_id);
                Some(join)
            }
            Err(error) => {
                warn!(%peer_id, %error, "session could not be attached");
                None
            }
        }
    }
}

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

#[instrument(skip_all)]
pub(crate) async fn handle_socket(
    socket: ProtoSocket,
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    sessions: Arc<SessionsHandle>,
    origin: SessionOrigin,
) {
    let dialed_by_server = matches!(origin, SessionOrigin::ServerDialed(_));
    info!(dialed_by_server, "WebSocket connection opened");
    let session = ProtoSession::new(socket, state, Arc::clone(&scheduler));
    let session = match tokio::time::timeout(HANDSHAKE_TIMEOUT, session.handshake(&origin)).await {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(_) => {
            warn!(
                timeout_secs = HANDSHAKE_TIMEOUT.as_secs(),
                dialed_by_server, "WebSocket handshake timed out; dropping connection"
            );
            if let SessionOrigin::ServerDialed(target) = &origin {
                scheduler.connection_failures.record(
                    &target.credentials.worker_id,
                    ConnectionDirection::Outbound,
                    false,
                    format!(
                        "handshake timed out after {} s",
                        HANDSHAKE_TIMEOUT.as_secs()
                    ),
                );
            }
            return;
        }
    };
    if let Some(join) = session.attach(&sessions).await {
        let _ = join.await;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AuthDecision {
    Accept,
    Reject { code: u16, reason: &'static str },
}

fn decide_auth(
    registered_peers_empty: bool,
    has_any_registrations: bool,
    authorized_peers_empty: bool,
    emptied_by_missing_cache: bool,
    is_base: bool,
) -> AuthDecision {
    if is_base {
        return match (authorized_peers_empty, emptied_by_missing_cache) {
            (false, _) => AuthDecision::Accept,
            // The projects did enable this worker but have no cache. A "not enabled" message would
            // send the operator to the wrong page.
            (true, true) => AuthDecision::Reject {
                code: 495,
                reason: "project has no cache subscribed",
            },
            (true, false) => AuthDecision::Reject {
                code: 403,
                reason: "base worker not enabled by any project",
            },
        };
    }

    if registered_peers_empty {
        return if has_any_registrations {
            AuthDecision::Reject {
                code: 403,
                reason: "worker is deactivated",
            }
        } else {
            AuthDecision::Reject {
                code: 403,
                reason: "unknown worker",
            }
        };
    }

    if authorized_peers_empty {
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

    #[test]
    fn inbound_unknown_worker_rejected() {
        let d = decide_auth(true, false, true, false, false);
        assert_eq!(
            d,
            AuthDecision::Reject {
                code: 403,
                reason: "unknown worker",
            }
        );
    }

    #[test]
    fn deactivated_worker_rejected_inbound() {
        assert_eq!(
            decide_auth(true, true, true, false, false),
            AuthDecision::Reject {
                code: 403,
                reason: "worker is deactivated",
            }
        );
    }

    #[test]
    fn registered_but_no_valid_token() {
        assert_eq!(
            decide_auth(false, false, true, false, false),
            AuthDecision::Reject {
                code: 401,
                reason: "no valid peer tokens provided",
            }
        );
    }

    #[test]
    fn registered_emptied_by_missing_cache() {
        assert_eq!(
            decide_auth(false, false, true, true, false),
            AuthDecision::Reject {
                code: 495,
                reason: "project has no cache subscribed",
            }
        );
    }

    #[test]
    fn base_worker_emptied_by_missing_cache() {
        assert_eq!(
            decide_auth(false, false, true, true, true),
            AuthDecision::Reject {
                code: 495,
                reason: "project has no cache subscribed",
            }
        );
    }

    #[test]
    fn registered_with_valid_token_accepted() {
        assert_eq!(
            decide_auth(false, false, false, false, false),
            AuthDecision::Accept
        );
    }

    #[test]
    fn base_worker_empty_authorized_rejected() {
        assert_eq!(
            decide_auth(false, false, true, false, true),
            AuthDecision::Reject {
                code: 403,
                reason: "base worker not enabled by any project",
            }
        );
    }

    #[test]
    fn base_worker_with_authorized_accepted() {
        assert_eq!(
            decide_auth(false, false, false, false, true),
            AuthDecision::Accept
        );
    }
}
