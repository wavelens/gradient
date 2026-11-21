/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;

use gradient_db::WorkerDb;
use gradient_types::RuntimeConfig;
use gradient_types::ids::ProjectId;
use tokio::task::JoinHandle;
use tracing::{debug, info, instrument, warn};

use anyhow::Result;
use async_trait::async_trait;

use gradient_scheduler::Scheduler;
use gradient_scheduler::connection_failures::ConnectionDirection;
use gradient_types::SecretString;
use gradient_wire::Rejection;
use gradient_wire::messages::GradientCapabilities;
use gradient_wire::session::handshake as handshake_fsm;
use gradient_wire::session::handshake::{HandshakeResult, Rejected};
use gradient_wire::traits::{AuthOutcome, PeerAuthority};

use super::SessionHandles;
use super::auth::{check_worker_tokens, load_inbound_worker_challenge};
use super::dialed::DialedWorkerAuthority;
use super::failures::{record_dialed, record_inbound};
use super::session_actor::SessionArgs;
use super::session_authorization::{HandshakeProof, derive_session_authorization};
use super::sessions::SessionsHandle;
use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket};
use crate::outbound::DialTarget;

pub(crate) enum SessionOrigin {
    WorkerDialed,
    ServerDialed(DialTarget),
}

pub(super) struct Opening;

pub(super) struct Authenticated {
    pub peer_id: String,
    pub advertised: GradientCapabilities,
    pub negotiated: GradientCapabilities,
    pub authorized_peers: Vec<String>,
    pub proof: HandshakeProof,
}

pub(super) struct ProtoSession<S> {
    pub socket: ProtoSocket,
    pub handles: SessionHandles,
    pub scheduler: Arc<Scheduler>,
    pub session_state: S,
}

impl ProtoSession<Opening> {
    pub fn new(socket: ProtoSocket, handles: SessionHandles, scheduler: Arc<Scheduler>) -> Self {
        Self {
            socket,
            handles,
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
        let (result, proof) = match outcome {
            Ok(proven) => proven,
            Err(e) => {
                debug!(error = %e, "handshake failed");
                return None;
            }
        };

        info!(peer_id = %result.peer_id, authorized = result.authorized_peers.len(), "handshake complete");
        Some(ProtoSession {
            socket: self.socket,
            handles: self.handles,
            scheduler: self.scheduler,
            session_state: Authenticated {
                peer_id: result.peer_id,
                advertised: result.advertised,
                negotiated: result.negotiated,
                authorized_peers: result.authorized_peers,
                proof,
            },
        })
    }

    async fn accept_worker(&mut self) -> Result<(HandshakeResult, HandshakeProof)> {
        if !self.handles.db.config.proto.discoverable {
            self.socket
                .send_reject(Rejection::NotAcceptingConnections)
                .await;
            anyhow::bail!(Rejection::NotAcceptingConnections);
        }

        let authority = ServerAuthority {
            db: self.handles.db.worker_db.clone(),
            config: Arc::clone(&self.handles.db.config),
            proven_peer_token_hashes: OnceLock::new(),
        };
        let outcome = handshake_fsm::as_authority(&mut self.socket, &authority).await;
        if let Err(error) = &outcome
            && let Some(rejected) = error.downcast_ref::<Rejected>()
        {
            let before_auth = !authority.proved_any_token();
            let failures = &self.scheduler.connection_failures;
            record_inbound(&self.handles.db.worker_db, failures, rejected, before_auth).await;
        }

        Ok((outcome?, authority.into_proof()))
    }

    async fn dial_worker(
        &mut self,
        target: &DialTarget,
    ) -> Result<(HandshakeResult, HandshakeProof)> {
        let authority = DialedWorkerAuthority {
            db: self.handles.db.worker_db.clone(),
            config: Arc::clone(&self.handles.db.config),
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

        Ok((outcome?, HandshakeProof::ServerDialed(authority.session)))
    }
}

struct ServerAuthority {
    db: WorkerDb,
    config: Arc<RuntimeConfig>,
    proven_peer_token_hashes: OnceLock<Vec<(String, String)>>,
}

impl ServerAuthority {
    fn proved_any_token(&self) -> bool {
        self.proven_peer_token_hashes
            .get()
            .is_some_and(|proven| !proven.is_empty())
    }

    fn into_proof(self) -> HandshakeProof {
        HandshakeProof::WorkerDialed {
            proven_peer_token_hashes: self
                .proven_peer_token_hashes
                .into_inner()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl PeerAuthority for ServerAuthority {
    type Challenge = Vec<(String, String)>;

    async fn challenge(&self, claimed: &str) -> Result<(Vec<(String, String)>, Vec<String>)> {
        let (_, registered_peers) = load_inbound_worker_challenge(&self.db, claimed).await?;
        let names = registered_peers.iter().map(|(id, _)| id.clone()).collect();
        Ok((registered_peers, names))
    }

    async fn authorize(
        &self,
        claimed: &str,
        registered_peers: Vec<(String, String)>,
        tokens: &[(String, SecretString)],
        advertised: GradientCapabilities,
    ) -> Result<AuthOutcome> {
        let checked = check_worker_tokens(&registered_peers, tokens).await?;
        if checked.proven_peer_token_hashes.is_empty() {
            return Ok(AuthOutcome::Reject(Rejection::UnknownWorkerOrWrongToken));
        }
        let proof = HandshakeProof::WorkerDialed {
            proven_peer_token_hashes: checked.proven_peer_token_hashes.clone(),
        };
        let _ = self
            .proven_peer_token_hashes
            .set(checked.proven_peer_token_hashes);

        derive_session_authorization(&self.db, &self.config, claimed, &proof, advertised)
            .await?
            .including_failed_peers(checked.failed_peers)
            .into_handshake_outcome()
    }
}

impl ProtoSession<Authenticated> {
    pub async fn attach(self, sessions: &SessionsHandle) -> Option<JoinHandle<()>> {
        let ProtoSession {
            mut socket,
            handles,
            scheduler,
            session_state:
                Authenticated {
                    peer_id,
                    advertised,
                    negotiated,
                    authorized_peers,
                    proof,
                },
        } = self;

        if scheduler.is_worker_connected(&peer_id).await {
            warn!(%peer_id, "duplicate connection rejected (worker already connected)");
            socket.send_reject(Rejection::AlreadyConnected).await;
            return None;
        }

        let failures = Arc::clone(&scheduler.connection_failures);
        let authorized_peers: HashSet<ProjectId> = authorized_peers
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        let args = SessionArgs {
            peer_id: peer_id.clone(),
            handles,
            scheduler,
            socket,
            advertised_capabilities: advertised,
            capabilities: negotiated,
            authorized_peers,
            proof,
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

#[instrument(skip_all)]
pub(crate) async fn handle_socket(
    socket: ProtoSocket,
    handles: SessionHandles,
    scheduler: Arc<Scheduler>,
    sessions: Arc<SessionsHandle>,
    origin: SessionOrigin,
) {
    let dialed_by_server = matches!(origin, SessionOrigin::ServerDialed(_));
    debug!(dialed_by_server, "WebSocket connection opened");
    let session = ProtoSession::new(socket, handles, Arc::clone(&scheduler));
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
pub(super) enum AuthDecision {
    Accept,
    Reject(Rejection),
}

pub(super) fn decide_auth(
    proven: bool,
    authorized_peers_empty: bool,
    emptied_by_missing_cache: bool,
) -> AuthDecision {
    match (proven, authorized_peers_empty, emptied_by_missing_cache) {
        (false, _, _) => AuthDecision::Reject(Rejection::UnknownWorkerOrWrongToken),
        (true, false, _) => AuthDecision::Accept,
        // The projects did enable this worker but have no cache. A "not enabled" message would
        // send the operator to the wrong page.
        (true, true, true) => AuthDecision::Reject(Rejection::NoCacheSubscribed),
        (true, true, false) => AuthDecision::Reject(Rejection::NoProjectGrant),
    }
}

#[cfg(test)]
mod auth_decision_tests {
    use super::{AuthDecision, Rejection, decide_auth};

    #[test]
    fn decide_auth_maps_every_input_to_the_right_decision() {
        let cases = [
            (
                (false, true, false),
                AuthDecision::Reject(Rejection::UnknownWorkerOrWrongToken),
            ),
            (
                (false, true, true),
                AuthDecision::Reject(Rejection::UnknownWorkerOrWrongToken),
            ),
            (
                (true, true, true),
                AuthDecision::Reject(Rejection::NoCacheSubscribed),
            ),
            (
                (true, true, false),
                AuthDecision::Reject(Rejection::NoProjectGrant),
            ),
            ((true, false, false), AuthDecision::Accept),
            ((true, false, true), AuthDecision::Accept),
        ];
        for ((proven, no_authorized, no_cache), expected) in cases {
            assert_eq!(decide_auth(proven, no_authorized, no_cache), expected);
        }
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;

    const SHA256_OF_T1: &str = "628b49d96dcde97a430dd4f597705899e09a968f793491e4b704cae33a40dc02";

    fn authority_for(db: sea_orm::DatabaseConnection) -> ServerAuthority {
        let state = gradient_test_support::prelude::test_state(db);
        ServerAuthority {
            db: state.worker_db.clone(),
            config: Arc::clone(&state.config),
            proven_peer_token_hashes: OnceLock::new(),
        }
    }

    fn t1_for(project: ProjectId) -> (String, SecretString) {
        (project.to_string(), SecretString::from("t1".to_owned()))
    }

    #[tokio::test]
    async fn a_rejection_after_a_valid_token_counts_as_proven() {
        let db = TestDatabase::create().await;
        insert::<EUser>(&db, rows::user()).await;
        insert::<EProject>(&db, rows::project()).await;
        insert_registration(&db, None).await;
        let authority = authority_for(db.connection());
        let (challenge, _) = authority.challenge(WORKER_ID).await.unwrap();

        let outcome = authority
            .authorize(
                WORKER_ID,
                challenge,
                &[t1_for(rows::project().id)],
                GradientCapabilities::default(),
            )
            .await
            .unwrap();

        assert_eq!(outcome, AuthOutcome::Reject(Rejection::NoCacheSubscribed));
        assert!(authority.proved_any_token());
        db.finish().await;
    }

    #[tokio::test]
    async fn the_handshake_proof_lists_only_the_registrations_whose_token_matched() {
        let db = database_with_a_cached_project().await;
        insert_registration(&db, None).await;
        let (proven, failed) = (rows::project().id, ProjectId::now_v7());
        let authority = authority_for(db.connection());
        let challenge = vec![
            (proven.to_string(), SHA256_OF_T1.into()),
            (failed.to_string(), SHA256_OF_T1.into()),
        ];

        authority
            .authorize(
                WORKER_ID,
                challenge,
                &[
                    t1_for(proven),
                    (failed.to_string(), SecretString::from("wrong".to_owned())),
                ],
                GradientCapabilities::default(),
            )
            .await
            .unwrap();

        let HandshakeProof::WorkerDialed {
            proven_peer_token_hashes,
        } = authority.into_proof()
        else {
            panic!("a worker-dialed handshake records a worker-dialed proof");
        };
        assert_eq!(
            proven_peer_token_hashes,
            vec![(proven.to_string(), SHA256_OF_T1.to_owned())]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn an_unknown_worker_id_and_a_wrong_token_get_the_same_rejection() {
        let project = ProjectId::now_v7().to_string();
        let authority = authority_for(mock_db().into_connection());
        let token = [(project.clone(), SecretString::from("wrong".to_owned()))];

        for challenge in [vec![], vec![(project.clone(), SHA256_OF_T1.into())]] {
            let outcome = authority
                .authorize("w1", challenge, &token, GradientCapabilities::default())
                .await
                .unwrap();
            assert_eq!(
                outcome,
                AuthOutcome::Reject(Rejection::UnknownWorkerOrWrongToken)
            );
        }
        assert!(!authority.proved_any_token());
    }

    use gradient_entity::project_cache::CacheSubscriptionMode;
    use gradient_entity::{team_project, team_worker, worker_registration};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_types::ids::{ProjectCacheId, TeamProjectId, TeamWorkerId};
    use gradient_types::{ECache, EProject, EProjectCache, ETeam, ETeamProject, EUser};
    use sea_orm::EntityTrait;

    const DIAL_URL: &str = "wss://servers.gradient.ci/proto";
    const WORKER_ID: &str = "a0000000-0000-0000-0000-000000000040";

    async fn insert<E>(db: &TestDatabase, row: E::Model)
    where
        E: EntityTrait,
        E::Model: sea_orm::IntoActiveModel<E::ActiveModel>,
        E::ActiveModel: sea_orm::ActiveModelBehavior + Send,
    {
        gradient_db::rows::insert::<E, _>(&db.connection(), row)
            .await
            .expect("the row is written");
    }

    async fn database_with_a_cached_project() -> TestDatabase {
        let db = TestDatabase::create().await;
        insert::<EUser>(&db, rows::user()).await;
        insert::<EProject>(&db, rows::project()).await;
        insert::<ECache>(&db, rows::cache()).await;
        insert::<EProjectCache>(
            &db,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                rows::project().id,
                rows::cache().id,
                CacheSubscriptionMode::ReadWrite,
            ),
        )
        .await;
        db
    }

    async fn insert_registration(db: &TestDatabase, url: Option<&str>) {
        insert::<worker_registration::Entity>(
            db,
            worker_registration::Model {
                peer_id: rows::project().id,
                worker_id: WORKER_ID.into(),
                token_hash: SHA256_OF_T1.into(),
                url: url.map(str::to_owned),
                gradient_ci: url.is_some(),
                created_by: None,
                ..rows::worker_registration(true, true)
            },
        )
        .await;
    }

    async fn inbound_handshake(db: &TestDatabase, peer: String) -> AuthOutcome {
        let authority = authority_for(db.connection());
        let (challenge, _) = authority.challenge(WORKER_ID).await.unwrap();
        authority
            .authorize(
                WORKER_ID,
                challenge,
                &[(peer, SecretString::from("t1".to_owned()))],
                GradientCapabilities::default(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_worker_dialed_handshake_is_refused_when_the_registrations_cannot_be_read() {
        use sea_orm::ConnectionTrait;
        let db = database_with_a_cached_project().await;
        insert_registration(&db, None).await;
        let authority = authority_for(db.connection());
        let (challenge, _) = authority.challenge(WORKER_ID).await.unwrap();
        let tokens = [t1_for(rows::project().id)];

        db.connection()
            .execute_unprepared(
                "ALTER TABLE worker_registration RENAME TO worker_registration_unreadable",
            )
            .await
            .unwrap();
        let outcome = authority
            .authorize(
                WORKER_ID,
                challenge,
                &tokens,
                GradientCapabilities::default(),
            )
            .await;
        db.connection()
            .execute_unprepared(
                "ALTER TABLE worker_registration_unreadable RENAME TO worker_registration",
            )
            .await
            .unwrap();

        assert!(
            outcome.is_err(),
            "expected a refused handshake, got {outcome:?}"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn inbound_connection_with_project_registration_credentials_is_refused() {
        let db = database_with_a_cached_project().await;
        insert_registration(&db, Some(DIAL_URL)).await;

        let outcome = inbound_handshake(&db, rows::project().id.to_string()).await;

        assert_eq!(
            outcome,
            AuthOutcome::Reject(Rejection::UnknownWorkerOrWrongToken)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn inbound_connection_with_server_dialed_team_worker_credentials_is_refused() {
        let db = database_with_a_cached_project().await;
        let team = rows::team();
        insert::<ETeam>(&db, team.clone()).await;
        insert::<ETeamProject>(
            &db,
            team_project::Model {
                id: TeamProjectId::now_v7(),
                team: team.id,
                project: rows::project().id,
                role: None,
                includes_users: false,
                includes_workers: true,
                grant_source: team_project::TeamGrantSource::Manual,
                created_at: gradient_types::now(),
            },
        )
        .await;
        insert::<team_worker::Entity>(
            &db,
            team_worker::Model {
                id: TeamWorkerId::now_v7(),
                team: team.id,
                worker_id: WORKER_ID.into(),
                token_hash: SHA256_OF_T1.into(),
                url: Some(DIAL_URL.into()),
                gradient_ci: true,
                enable_eval: true,
                enable_build: true,
                active: true,
                created_at: gradient_types::now(),
                ..Default::default()
            },
        )
        .await;

        let outcome = inbound_handshake(&db, team.id.to_string()).await;

        assert_eq!(
            outcome,
            AuthOutcome::Reject(Rejection::UnknownWorkerOrWrongToken)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn inbound_worker_without_dial_url_passes_project_handshake() {
        let db = database_with_a_cached_project().await;
        insert_registration(&db, None).await;

        let outcome = inbound_handshake(&db, rows::project().id.to_string()).await;

        assert_eq!(
            outcome,
            AuthOutcome::Accept {
                authorized_peers: vec![rows::project().id.to_string()],
                failed_peers: vec![],
                granted_capabilities: GradientCapabilities {
                    core: true,
                    cache: true,
                    ..GradientCapabilities::default()
                },
            }
        );
        db.finish().await;
    }
}
