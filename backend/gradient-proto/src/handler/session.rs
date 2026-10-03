/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    TeamWorkerChallenge, aggregate_enabled_caps, challenge_for, has_any_registrations,
    negotiate_capabilities, resolve_authorized,
};
use super::dialed::{DialedSession, DialedWorkerAuthority};
use super::failures::{record_dialed, record_inbound};
use super::session_actor::SessionArgs;
use super::sessions::SessionsHandle;
use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, send_server_msg};
use crate::outbound::DialTarget;

pub(crate) enum SessionOrigin {
    WorkerDialed,
    ServerDialed(DialTarget),
}

impl SessionOrigin {
    fn dialed_session(&self) -> Option<DialedSession> {
        match self {
            SessionOrigin::WorkerDialed => None,
            SessionOrigin::ServerDialed(target) => Some(target.session()),
        }
    }
}

pub(super) struct Opening;

pub(super) struct Authenticated {
    pub peer_id: String,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
    pub dialed: Option<DialedSession>,
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
                dialed: origin.dialed_session(),
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
            proven: AtomicBool::new(false),
        };
        let outcome = handshake_fsm::as_authority(&mut self.socket, &authority).await;
        if let Err(error) = &outcome
            && let Some(rejected) = error.downcast_ref::<Rejected>()
        {
            let before_auth = !authority.proven.load(Ordering::Relaxed);
            let failures = &self.scheduler.connection_failures;
            record_inbound(&self.state, failures, rejected, before_auth).await;
        }

        outcome
    }

    async fn dial_worker(&mut self, target: &DialTarget) -> Result<HandshakeResult> {
        let authority = DialedWorkerAuthority {
            state: Arc::clone(&self.state),
            session: target.session(),
        };
        let outcome =
            handshake_fsm::as_dialer(&mut self.socket, &target.credentials, &authority).await;
        if let Err(error) = &outcome {
            record_dialed(
                &self.scheduler.connection_failures,
                &target.credentials.worker_id,
                error,
            );
        }

        outcome
    }
}

struct ServerAuthority {
    state: Arc<ServerState>,
    proven: AtomicBool,
}

struct ServerChallenge {
    team: Option<TeamWorkerChallenge>,
    registered_peers: Vec<(String, String)>,
}

#[async_trait]
impl PeerAuthority for ServerAuthority {
    type Challenge = ServerChallenge;

    async fn challenge(&self, claimed: &str) -> Result<(ServerChallenge, Vec<String>)> {
        let (team, registered_peers) = challenge_for(&self.state, claimed).await;
        let names = registered_peers.iter().map(|(id, _)| id.clone()).collect();
        Ok((
            ServerChallenge {
                team,
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
            team,
            registered_peers,
        } = challenge;
        let resolved = resolve_authorized(&self.state, &team, &registered_peers, tokens).await;
        self.proven.store(resolved.proven, Ordering::Relaxed);

        let has_any =
            registered_peers.is_empty() && has_any_registrations(&self.state, claimed).await;
        match decide_auth(
            registered_peers.is_empty(),
            has_any,
            resolved.authorized.is_empty(),
            resolved.emptied_by_missing_cache,
            team.is_some(),
        ) {
            AuthDecision::Accept => Ok(AuthOutcome::Accept {
                authorized_peers: resolved.authorized,
                failed_peers: resolved.failed,
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
                    dialed,
                },
        } = self;

        if scheduler.is_worker_connected(&peer_id).await {
            warn!(%peer_id, "duplicate connection rejected (worker already connected)");
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
            dialed,
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
    let (team, registered_peers) = challenge_for(state, peer_id).await;
    if team.is_none() && registered_peers.is_empty() && has_any_registrations(state, peer_id).await
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
    is_team: bool,
) -> AuthDecision {
    if is_team {
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
                reason: "no project grants this team's workers",
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
    fn team_worker_emptied_by_missing_cache() {
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
    fn a_team_worker_without_granted_projects_is_refused() {
        assert_eq!(
            decide_auth(false, false, true, false, true),
            AuthDecision::Reject {
                code: 403,
                reason: "no project grants this team's workers",
            }
        );
    }

    #[test]
    fn team_worker_with_authorized_accepted() {
        assert_eq!(
            decide_auth(false, false, false, false, true),
            AuthDecision::Accept
        );
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    use gradient_entity::project::Model as ProjectModel;
    use gradient_entity::project_cache::Model as ProjectCacheModel;
    use sea_orm::{DatabaseBackend, MockDatabase};

    const SHA256_OF_T1: &str = "628b49d96dcde97a430dd4f597705899e09a968f793491e4b704cae33a40dc02";

    #[tokio::test]
    async fn a_rejection_after_a_valid_token_counts_as_proven() {
        let project = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![ProjectModel {
                id: project,
                ..Default::default()
            }]])
            .append_query_results([Vec::<ProjectCacheModel>::new()]);
        let authority = ServerAuthority {
            state: gradient_test_support::prelude::test_state(db.into_connection()),
            proven: AtomicBool::new(false),
        };
        let challenge = ServerChallenge {
            team: None,
            registered_peers: vec![(project.to_string(), SHA256_OF_T1.into())],
        };

        let outcome = authority
            .authorize("w1", challenge, &[(project.to_string(), "t1".into())])
            .await
            .unwrap();

        assert!(matches!(outcome, AuthOutcome::Reject { code: 495, .. }));
        assert!(authority.proven.load(Ordering::Relaxed));
    }
}
