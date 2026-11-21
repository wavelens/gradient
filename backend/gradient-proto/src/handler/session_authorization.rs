/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use gradient_db::WorkerDb;
use gradient_scheduler::connection_failures::ConnectionDirection;
use gradient_types::RuntimeConfig;
use gradient_types::ids::ProjectId;
use gradient_wire::Rejection;
use gradient_wire::messages::{FailedPeer, GradientCapabilities};
use gradient_wire::traits::AuthOutcome;
use sea_orm::DbErr;

use super::auth::{
    aggregate_enabled_caps, expand_team_authorized, filter_project_peers_without_cache,
    load_inbound_worker_challenge, negotiate_capabilities,
};
use super::dialed::{DialedSession, rejection_of_dialed_worker};
use super::session::{AuthDecision, decide_auth};
use crate::outbound::{Dialable, dialable};

/// What a session proved at the handshake: the registrations whose token hash it matched, or that
/// the server dialed the worker.
#[derive(Clone, Debug)]
pub enum HandshakeProof {
    WorkerDialed {
        proven_peer_token_hashes: Vec<(String, String)>,
    },
    ServerDialed(DialedSession),
}

impl HandshakeProof {
    pub(crate) fn dialed_session(&self) -> Option<&DialedSession> {
        match self {
            HandshakeProof::WorkerDialed { .. } => None,
            HandshakeProof::ServerDialed(session) => Some(session),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum SessionAuthorization {
    Authorized {
        projects: HashSet<ProjectId>,
        failed_peers: Vec<FailedPeer>,
        capabilities: GradientCapabilities,
    },
    RedialForUncoveredProject {
        project: String,
    },
    Rejected(Rejection),
}

impl SessionAuthorization {
    pub(crate) fn including_failed_peers(mut self, failed: Vec<FailedPeer>) -> Self {
        if let SessionAuthorization::Authorized { failed_peers, .. } = &mut self {
            for peer in failed {
                if !failed_peers.contains(&peer) {
                    failed_peers.push(peer);
                }
            }
        }
        self
    }

    pub(crate) fn into_handshake_outcome(
        self,
        user_url_destination_allowlist: Vec<String>,
    ) -> anyhow::Result<AuthOutcome> {
        match self {
            SessionAuthorization::Authorized {
                projects,
                failed_peers,
                capabilities,
            } => {
                let mut authorized_peers: Vec<String> =
                    projects.iter().map(ToString::to_string).collect();
                authorized_peers.sort();
                Ok(AuthOutcome::Accept {
                    authorized_peers,
                    failed_peers,
                    granted_capabilities: capabilities,
                    user_url_destination_allowlist,
                })
            }
            SessionAuthorization::Rejected(rejection) => Ok(AuthOutcome::Reject(rejection)),
            SessionAuthorization::RedialForUncoveredProject { project } => anyhow::bail!(
                "project {project} was registered during the handshake; the worker is dialed again with its token"
            ),
        }
    }
}

pub(crate) async fn derive_session_authorization(
    db: &WorkerDb,
    config: &RuntimeConfig,
    worker_id: &str,
    proof: &HandshakeProof,
    advertised: GradientCapabilities,
) -> Result<SessionAuthorization, DbErr> {
    match proof {
        HandshakeProof::WorkerDialed {
            proven_peer_token_hashes,
        } => {
            derive_worker_dialed_session_authorization(
                db,
                config,
                worker_id,
                proven_peer_token_hashes,
                advertised,
            )
            .await
        }
        HandshakeProof::ServerDialed(session) => {
            derive_server_dialed_session_authorization(db, config, worker_id, session, advertised)
                .await
        }
    }
}

async fn derive_worker_dialed_session_authorization(
    db: &WorkerDb,
    config: &RuntimeConfig,
    worker_id: &str,
    proven_peer_token_hashes: &[(String, String)],
    advertised: GradientCapabilities,
) -> Result<SessionAuthorization, DbErr> {
    let (team, registered) = load_inbound_worker_challenge(db, worker_id).await?;
    if registered.is_empty() {
        return Ok(SessionAuthorization::Rejected(Rejection::WorkerDeactivated));
    }

    let still_proven: Vec<String> = registered
        .into_iter()
        .filter(|registered_pair| proven_peer_token_hashes.contains(registered_pair))
        .map(|(peer_id, _)| peer_id)
        .collect();
    let proven = !still_proven.is_empty();
    let expanded = expand_team_authorized(&team, still_proven);
    let had_any = !expanded.is_empty();
    let (authorized, failed_peers) = filter_project_peers_without_cache(db, expanded).await?;
    let emptied_by_missing_cache = authorized.is_empty() && had_any && !failed_peers.is_empty();

    if let AuthDecision::Reject(rejection) =
        decide_auth(proven, authorized.is_empty(), emptied_by_missing_cache)
    {
        return Ok(SessionAuthorization::Rejected(rejection));
    }

    let enabled = aggregate_enabled_caps(db, worker_id, ConnectionDirection::Inbound).await?;
    Ok(SessionAuthorization::Authorized {
        projects: only_projects(&authorized),
        failed_peers,
        capabilities: negotiate_capabilities(config, advertised, enabled),
    })
}

async fn derive_server_dialed_session_authorization(
    db: &WorkerDb,
    config: &RuntimeConfig,
    worker_id: &str,
    session: &DialedSession,
    advertised: GradientCapabilities,
) -> Result<SessionAuthorization, DbErr> {
    let current = match dialable(db, worker_id, &session.url).await? {
        Dialable::Projects { projects, .. } => projects,
        Dialable::Refused(rejection) => return Ok(SessionAuthorization::Rejected(rejection)),
    };
    if !session.team_worker
        && let Some(project) = current
            .iter()
            .find(|project| !session.token_projects.contains(project))
    {
        return Ok(SessionAuthorization::RedialForUncoveredProject {
            project: project.clone(),
        });
    }

    let (authorized, failed_peers) = filter_project_peers_without_cache(db, current).await?;
    if let Some(rejection) = rejection_of_dialed_worker(&authorized, &failed_peers) {
        return Ok(SessionAuthorization::Rejected(rejection));
    }

    let enabled = aggregate_enabled_caps(db, worker_id, ConnectionDirection::Outbound).await?;
    Ok(SessionAuthorization::Authorized {
        projects: only_projects(&authorized),
        failed_peers,
        capabilities: negotiate_capabilities(config, advertised, enabled),
    })
}

fn only_projects(peers: &[String]) -> HashSet<ProjectId> {
    peers.iter().filter_map(|peer| peer.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::dialed::DialedWorkerAuthority;
    use gradient_entity::project_cache::CacheSubscriptionMode;
    use gradient_entity::{team_project, team_worker, worker_registration};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_types::ids::{ProjectCacheId, ProjectId, TeamProjectId, TeamWorkerId};
    use gradient_types::{ECache, EProject, EProjectCache, ETeam, ETeamProject, EUser};
    use gradient_wire::traits::DialerAuthority;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
    use std::collections::HashSet;

    const WORKER_ID: &str = "worker-of-the-derive-tests";
    const TOKEN_HASH: &str = "628b49d96dcde97a430dd4f597705899e09a968f793491e4b704cae33a40dc02";
    const ROTATED_TOKEN_HASH: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";
    const DIAL_URL: &str = "wss://worker-of-the-derive-tests.example/proto";

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

    async fn database_with_cached_projects(count: usize) -> (TestDatabase, Vec<ProjectId>) {
        let db = TestDatabase::create().await;
        insert::<EUser>(&db, rows::user()).await;
        insert::<ECache>(&db, rows::cache()).await;
        let mut projects = Vec::new();
        for index in 0..count {
            let project = ProjectId::now_v7();
            insert::<EProject>(
                &db,
                gradient_entity::project::Model {
                    id: project,
                    name: format!("derive-project-{index}"),
                    ..rows::project()
                },
            )
            .await;
            insert::<EProjectCache>(
                &db,
                rows::project_cache_link(
                    ProjectCacheId::now_v7(),
                    project,
                    rows::cache().id,
                    CacheSubscriptionMode::ReadWrite,
                ),
            )
            .await;
            projects.push(project);
        }
        (db, projects)
    }

    fn registration(project: ProjectId, token_hash: &str) -> worker_registration::Model {
        worker_registration::Model {
            peer_id: project,
            worker_id: WORKER_ID.into(),
            token_hash: token_hash.into(),
            url: None,
            created_by: None,
            ..rows::worker_registration(true, true)
        }
    }

    fn dialed_registration(project: ProjectId) -> worker_registration::Model {
        worker_registration::Model {
            url: Some(DIAL_URL.into()),
            token_encrypted: Some("stored-token".into()),
            ..registration(project, TOKEN_HASH)
        }
    }

    fn team_worker_row(active: bool) -> team_worker::Model {
        team_worker::Model {
            id: TeamWorkerId::now_v7(),
            team: rows::team().id,
            worker_id: WORKER_ID.into(),
            token_hash: TOKEN_HASH.into(),
            url: None,
            enable_eval: true,
            enable_build: true,
            active,
            created_at: gradient_types::now(),
            ..Default::default()
        }
    }

    async fn insert_team_worker_row(
        db: &TestDatabase,
        granted: &[ProjectId],
        worker: team_worker::Model,
    ) {
        insert::<ETeam>(db, rows::team()).await;
        for project in granted {
            insert::<ETeamProject>(db, team_grant(*project)).await;
        }
        insert::<team_worker::Entity>(db, worker).await;
    }

    async fn insert_team_worker(db: &TestDatabase, granted: &[ProjectId], active: bool) {
        insert_team_worker_row(db, granted, team_worker_row(active)).await;
    }

    async fn insert_dialed_team_worker(db: &TestDatabase, granted: &[ProjectId]) {
        let worker = team_worker::Model {
            url: Some(DIAL_URL.into()),
            token_encrypted: Some("stored-token".into()),
            ..team_worker_row(true)
        };
        insert_team_worker_row(db, granted, worker).await;
    }

    fn dialed_session(projects: &[ProjectId], team_worker: bool) -> DialedSession {
        DialedSession {
            url: DIAL_URL.into(),
            token_projects: projects.iter().map(ToString::to_string).collect(),
            team_worker,
        }
    }

    async fn derive_dialed(db: &TestDatabase, session: &DialedSession) -> SessionAuthorization {
        let state = gradient_test_support::prelude::test_state(db.connection());
        derive_session_authorization(
            &state.worker_db,
            &state.config,
            WORKER_ID,
            &HandshakeProof::ServerDialed(session.clone()),
            every_capability(),
        )
        .await
        .expect("the database answers")
    }

    fn team_grant(project: ProjectId) -> team_project::Model {
        team_project::Model {
            id: TeamProjectId::now_v7(),
            team: rows::team().id,
            project,
            role: None,
            includes_users: false,
            includes_workers: true,
            grant_source: team_project::TeamGrantSource::Manual,
            created_at: gradient_types::now(),
        }
    }

    fn every_capability() -> GradientCapabilities {
        GradientCapabilities {
            core: true,
            cache: true,
            federate: true,
            eval: true,
            build: true,
            network: true,
            secrets: true,
            exec: true,
        }
    }

    fn proof_for(peers: &[String]) -> Vec<(String, String)> {
        peers
            .iter()
            .map(|peer| (peer.clone(), TOKEN_HASH.to_owned()))
            .collect()
    }

    async fn derive(db: &TestDatabase, proven: &[(String, String)]) -> SessionAuthorization {
        let state = gradient_test_support::prelude::test_state(db.connection());
        let proof = HandshakeProof::WorkerDialed {
            proven_peer_token_hashes: proven.to_vec(),
        };
        derive_session_authorization(
            &state.worker_db,
            &state.config,
            WORKER_ID,
            &proof,
            every_capability(),
        )
        .await
        .expect("the database answers")
    }

    async fn set_registrations_readable(db: &TestDatabase, readable: bool) {
        use sea_orm::ConnectionTrait;
        let sql = if readable {
            "ALTER TABLE worker_registration_unreadable RENAME TO worker_registration"
        } else {
            "ALTER TABLE worker_registration RENAME TO worker_registration_unreadable"
        };
        db.connection().execute_unprepared(sql).await.unwrap();
    }

    #[tokio::test]
    async fn a_server_dialed_handshake_is_refused_when_the_registrations_cannot_be_read() {
        let (db, projects) = database_with_cached_projects(1).await;
        insert::<worker_registration::Entity>(&db, dialed_registration(projects[0])).await;
        let state = gradient_test_support::prelude::test_state(db.connection());
        let authority = DialedWorkerAuthority {
            db: state.worker_db.clone(),
            config: std::sync::Arc::clone(&state.config),
            session: dialed_session(&projects, false),
            user_url_destination_allowlist: vec![],
        };
        assert!(matches!(
            authority.admit(WORKER_ID, every_capability()).await,
            Ok(AuthOutcome::Accept { .. })
        ));

        set_registrations_readable(&db, false).await;
        let outcome = authority.admit(WORKER_ID, every_capability()).await;
        set_registrations_readable(&db, true).await;

        assert!(
            outcome.is_err(),
            "expected a refused handshake, got {outcome:?}"
        );
        db.finish().await;
    }

    fn authorized_projects(decision: SessionAuthorization) -> HashSet<ProjectId> {
        match decision {
            SessionAuthorization::Authorized { projects, .. } => projects,
            other => panic!("expected an authorized session, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn derive_rejects_a_dialed_team_worker_whose_last_project_grant_is_removed() {
        let (db, projects) = database_with_cached_projects(1).await;
        insert_dialed_team_worker(&db, &projects).await;
        let session = dialed_session(&projects, true);
        authorized_projects(derive_dialed(&db, &session).await);
        team_grant_of(&db, projects[0])
            .await
            .into_active_model()
            .delete(&db.connection())
            .await
            .expect("the grant is deleted");

        let decision = derive_dialed(&db, &session).await;

        assert_eq!(
            decision,
            SessionAuthorization::Rejected(Rejection::NoProjectGrant)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_drops_a_capability_switched_off_for_a_dialed_worker() {
        let (db, projects) = database_with_cached_projects(1).await;
        insert::<worker_registration::Entity>(
            &db,
            worker_registration::Model {
                enable_exec: false,
                ..dialed_registration(projects[0])
            },
        )
        .await;

        let decision = derive_dialed(&db, &dialed_session(&projects, false)).await;

        let SessionAuthorization::Authorized { capabilities, .. } = decision else {
            panic!("expected an authorized session, got {decision:?}");
        };
        assert!(!capabilities.exec);
        assert!(capabilities.build);
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_adds_a_project_newly_granted_to_a_dialed_team_worker() {
        let (db, projects) = database_with_cached_projects(2).await;
        insert_dialed_team_worker(&db, &projects[..1]).await;
        insert::<ETeamProject>(&db, team_grant(projects[1])).await;

        let decision = derive_dialed(&db, &dialed_session(&projects[..1], true)).await;

        assert_eq!(
            authorized_projects(decision),
            projects.iter().copied().collect::<HashSet<_>>()
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_asks_for_a_redial_when_a_dialed_worker_gains_a_project_outside_its_tokens() {
        let (db, projects) = database_with_cached_projects(2).await;
        for project in &projects {
            insert::<worker_registration::Entity>(&db, dialed_registration(*project)).await;
        }

        let decision = derive_dialed(&db, &dialed_session(&projects[..1], false)).await;

        assert_eq!(
            decision,
            SessionAuthorization::RedialForUncoveredProject {
                project: projects[1].to_string()
            }
        );
        db.finish().await;
    }

    async fn team_grant_of(db: &TestDatabase, project: ProjectId) -> team_project::Model {
        ETeamProject::find()
            .filter(team_project::Column::Project.eq(project))
            .one(&db.connection())
            .await
            .expect("the database answers")
            .expect("the grant exists")
    }

    #[tokio::test]
    async fn derive_deactivates_a_worker_without_registration_rows() {
        let (db, projects) = database_with_cached_projects(1).await;

        let decision = derive(&db, &proof_for(&[projects[0].to_string()])).await;

        assert_eq!(
            decision,
            SessionAuthorization::Rejected(Rejection::WorkerDeactivated)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_drops_the_project_whose_token_was_rotated() {
        let (db, projects) = database_with_cached_projects(2).await;
        insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
        insert::<worker_registration::Entity>(&db, registration(projects[1], ROTATED_TOKEN_HASH))
            .await;
        let peers: Vec<String> = projects.iter().map(ToString::to_string).collect();

        let decision = derive(&db, &proof_for(&peers)).await;

        assert_eq!(authorized_projects(decision), HashSet::from([projects[0]]));
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_drops_secrets_when_a_registration_disables_them() {
        let (db, projects) = database_with_cached_projects(1).await;
        insert::<worker_registration::Entity>(
            &db,
            worker_registration::Model {
                enable_secrets: false,
                ..registration(projects[0], TOKEN_HASH)
            },
        )
        .await;

        let decision = derive(&db, &proof_for(&[projects[0].to_string()])).await;

        let SessionAuthorization::Authorized { capabilities, .. } = decision else {
            panic!("expected an authorized session, got {decision:?}");
        };
        assert!(!capabilities.secrets);
        assert!(capabilities.build);
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_rejects_an_inactive_team_worker() {
        let (db, projects) = database_with_cached_projects(1).await;
        insert_team_worker(&db, &projects, false).await;

        let decision = derive(&db, &proof_for(&[rows::team().id.to_string()])).await;

        assert_eq!(
            decision,
            SessionAuthorization::Rejected(Rejection::WorkerDeactivated)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_removes_the_project_of_a_removed_team_grant() {
        let (db, projects) = database_with_cached_projects(2).await;
        insert_team_worker(&db, &projects[..1], true).await;
        let removed = team_grant(projects[1]);
        insert::<ETeamProject>(&db, removed.clone()).await;
        removed
            .into_active_model()
            .delete(&db.connection())
            .await
            .expect("the grant is deleted");

        let decision = derive(&db, &proof_for(&[rows::team().id.to_string()])).await;

        assert_eq!(authorized_projects(decision), HashSet::from([projects[0]]));
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_never_grants_every_project_to_a_team_worker_without_projects() {
        let (db, _) = database_with_cached_projects(1).await;
        insert_team_worker(&db, &[], true).await;

        let decision = derive(&db, &proof_for(&[rows::team().id.to_string()])).await;

        assert_eq!(
            decision,
            SessionAuthorization::Rejected(Rejection::NoProjectGrant)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn derive_deactivates_a_worker_dialed_session_whose_registrations_are_all_server_dialed()
    {
        let (db, projects) = database_with_cached_projects(1).await;
        insert::<worker_registration::Entity>(&db, dialed_registration(projects[0])).await;

        let decision = derive(&db, &proof_for(&[projects[0].to_string()])).await;

        assert_eq!(
            decision,
            SessionAuthorization::Rejected(Rejection::WorkerDeactivated)
        );
        db.finish().await;
    }

    mod reauth {
        use super::*;
        use crate::handler::SessionHandles;
        use crate::handler::session_actor::tests::connected_pair;
        use crate::handler::session_actor::{SessionActor, SessionArgs, SessionMsg};
        use futures::{SinkExt, StreamExt};
        use gradient_pool::session_port::SessionSignal;
        use gradient_scheduler::Scheduler;
        use gradient_types::SecretString;
        use gradient_wire::PROTO_VERSIONS;
        use gradient_wire::codec::{from_bytes, to_bytes};
        use gradient_wire::messages::{ClientMessage, ServerMessage};
        use ractor::{Actor, ActorRef};
        use std::sync::Arc;
        use tokio::net::TcpStream;
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

        type WorkerSide = WebSocketStream<MaybeTlsStream<TcpStream>>;

        fn worker_dialed_proof(projects: &[ProjectId]) -> HandshakeProof {
            let peers: Vec<String> = projects.iter().map(ToString::to_string).collect();
            HandshakeProof::WorkerDialed {
                proven_peer_token_hashes: proof_for(&peers),
            }
        }

        fn auth_response_with_valid_tokens_for(projects: &[ProjectId]) -> ClientMessage {
            ClientMessage::AuthResponse {
                tokens: projects
                    .iter()
                    .map(|project| (project.to_string(), SecretString::from("t1".to_owned())))
                    .collect(),
            }
        }

        async fn send_worker_message(worker: &mut WorkerSide, msg: ClientMessage) {
            let bytes = to_bytes(&msg, *PROTO_VERSIONS.end()).unwrap();
            worker.send(Message::Binary(bytes)).await.unwrap();
        }

        fn authorized_peers_of(msg: ServerMessage) -> HashSet<String> {
            match msg {
                ServerMessage::AuthUpdate {
                    authorized_peers, ..
                } => authorized_peers.into_iter().collect(),
                other => panic!("expected an AuthUpdate, got {other:?}"),
            }
        }

        async fn open_session(
            db: &TestDatabase,
            project: ProjectId,
        ) -> (Arc<Scheduler>, ActorRef<SessionMsg>, WorkerSide) {
            open_session_with_proof(db, project, worker_dialed_proof(&[project])).await
        }

        async fn open_session_with_proof(
            db: &TestDatabase,
            project: ProjectId,
            proof: HandshakeProof,
        ) -> (Arc<Scheduler>, ActorRef<SessionMsg>, WorkerSide) {
            open_session_advertising(db, project, proof, every_capability()).await
        }

        async fn open_session_advertising(
            db: &TestDatabase,
            project: ProjectId,
            proof: HandshakeProof,
            advertised: GradientCapabilities,
        ) -> (Arc<Scheduler>, ActorRef<SessionMsg>, WorkerSide) {
            let (socket, worker) = connected_pair().await;
            let state = gradient_test_support::prelude::test_state(db.connection());
            let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
            scheduler.spawn_core(None).await.unwrap();
            let (session, _) = Actor::spawn(
                None,
                SessionActor,
                SessionArgs {
                    peer_id: WORKER_ID.into(),
                    handles: SessionHandles::from_state(&state),
                    scheduler: Arc::clone(&scheduler),
                    socket,
                    advertised_capabilities: advertised.clone(),
                    capabilities: advertised,
                    authorized_peers: HashSet::from([project]),
                    proof,
                },
            )
            .await
            .unwrap();
            assert!(
                scheduler
                    .worker_authorized_for_project(WORKER_ID, project)
                    .await
            );
            (scheduler, session, worker)
        }

        async fn next_server_message(worker: &mut WorkerSide) -> ServerMessage {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(10), worker.next())
                .await
                .expect("a frame within 10 seconds")
                .expect("a frame")
                .expect("no transport error");
            let Message::Binary(bytes) = frame else {
                panic!("expected a binary frame, got {frame:?}");
            };
            from_bytes::<ServerMessage>(bytes, *PROTO_VERSIONS.end()).unwrap()
        }

        #[derive(Default)]
        struct SchedulerEntryWhenRejectionIsSent {
            scheduler: std::sync::OnceLock<(Arc<Scheduler>, ProjectId)>,
            authorized_for_project: std::sync::Mutex<Option<bool>>,
        }

        struct RecordSchedulerEntryWhenRejectionIsSent(Arc<SchedulerEntryWhenRejectionIsSent>);

        struct SentMessage(Option<String>);

        impl tracing::field::Visit for SentMessage {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "msg" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S>
            for RecordSchedulerEntryWhenRejectionIsSent
        {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut sent = SentMessage(None);
                event.record(&mut sent);
                let rejection = format!("{:?}", ServerMessage::from(Rejection::WorkerDeactivated));
                let Some((scheduler, project)) = self.0.scheduler.get() else {
                    return;
                };
                if sent.0.as_deref() != Some(rejection.as_str()) {
                    return;
                }
                let authorized = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(scheduler.worker_authorized_for_project(WORKER_ID, *project))
                });
                *self.0.authorized_for_project.lock().unwrap() = Some(authorized);
            }
        }

        #[test]
        fn a_reauth_after_the_registration_is_deleted_rejects_without_a_worker_frame() {
            use tracing_subscriber::layer::SubscriberExt;

            let entry = Arc::new(SchedulerEntryWhenRejectionIsSent::default());
            let dispatch = tracing::Dispatch::new(
                tracing_subscriber::Registry::default()
                    .with(RecordSchedulerEntryWhenRejectionIsSent(Arc::clone(&entry))),
            );
            let worker_thread_dispatch = dispatch.clone();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .on_thread_start(move || {
                    std::mem::forget(tracing::dispatcher::set_default(&worker_thread_dispatch));
                })
                .build()
                .unwrap();
            let _main_thread_dispatch = tracing::dispatcher::set_default(&dispatch);

            runtime.block_on(async {
                let (db, projects) = database_with_cached_projects(1).await;
                insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH))
                    .await;
                let (scheduler, session, mut worker) = open_session(&db, projects[0]).await;
                entry
                    .scheduler
                    .set((Arc::clone(&scheduler), projects[0]))
                    .ok()
                    .unwrap();

                worker_registration::Entity::delete_many()
                    .filter(worker_registration::Column::WorkerId.eq(WORKER_ID))
                    .exec(&db.connection())
                    .await
                    .unwrap();
                session
                    .send_message(SessionMsg::Signal(SessionSignal::Reauth))
                    .unwrap();

                assert_eq!(
                    next_server_message(&mut worker).await,
                    ServerMessage::from(Rejection::WorkerDeactivated)
                );
                assert_eq!(*entry.authorized_for_project.lock().unwrap(), Some(false));
                db.finish().await;
            });
        }

        #[tokio::test]
        async fn a_reauth_after_a_capability_is_switched_off_removes_it_from_the_scheduler() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let (scheduler, session, mut worker) = open_session(&db, projects[0]).await;

            let mut row = worker_registration::Entity::find()
                .filter(worker_registration::Column::WorkerId.eq(WORKER_ID))
                .one(&db.connection())
                .await
                .unwrap()
                .expect("the registration row")
                .into_active_model();
            row.enable_exec = sea_orm::Set(false);
            row.update(&db.connection()).await.unwrap();
            session
                .send_message(SessionMsg::Signal(SessionSignal::Reauth))
                .unwrap();

            assert_eq!(
                next_server_message(&mut worker).await,
                ServerMessage::AuthUpdate {
                    authorized_peers: vec![projects[0].to_string()],
                    failed_peers: Vec::new(),
                }
            );
            let capabilities = scheduler
                .worker_gradient_caps(WORKER_ID)
                .await
                .expect("the worker is registered");
            assert!(!capabilities.exec);
            assert!(capabilities.build);
            db.finish().await;
        }

        async fn set_exec_enabled(db: &TestDatabase, enabled: bool) {
            let mut row = worker_registration::Entity::find()
                .filter(worker_registration::Column::WorkerId.eq(WORKER_ID))
                .one(&db.connection())
                .await
                .unwrap()
                .expect("the registration row")
                .into_active_model();
            row.enable_exec = sea_orm::Set(enabled);
            row.update(&db.connection()).await.unwrap();
        }

        async fn scheduler_grants_exec(scheduler: &Scheduler) -> bool {
            scheduler
                .worker_gradient_caps(WORKER_ID)
                .await
                .expect("the worker is registered")
                .exec
        }

        #[tokio::test]
        async fn a_capability_switched_off_and_on_again_comes_back_on_the_next_recheck() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let (scheduler, session, _worker) = open_session(&db, projects[0]).await;

            set_exec_enabled(&db, false).await;
            assert!(recheck_keeps_session(&session).await);
            assert!(!scheduler_grants_exec(&scheduler).await);

            set_exec_enabled(&db, true).await;
            assert!(recheck_keeps_session(&session).await);
            assert!(scheduler_grants_exec(&scheduler).await);
            db.finish().await;
        }

        #[tokio::test]
        async fn a_recheck_never_grants_a_capability_the_worker_did_not_advertise() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let without_exec = GradientCapabilities {
                exec: false,
                ..every_capability()
            };
            let proof = worker_dialed_proof(&[projects[0]]);
            let (scheduler, session, _worker) =
                open_session_advertising(&db, projects[0], proof, without_exec).await;

            assert!(recheck_keeps_session(&session).await);
            assert!(!scheduler_grants_exec(&scheduler).await);
            db.finish().await;
        }

        #[tokio::test]
        async fn a_reauth_request_after_the_registration_is_deleted_rejects_and_leaves_no_project()
        {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let (scheduler, _session, mut worker) = open_session(&db, projects[0]).await;

            worker_registration::Entity::delete_many()
                .filter(worker_registration::Column::WorkerId.eq(WORKER_ID))
                .exec(&db.connection())
                .await
                .unwrap();
            send_worker_message(&mut worker, ClientMessage::ReauthRequest).await;

            assert_eq!(
                next_server_message(&mut worker).await,
                ServerMessage::from(Rejection::WorkerDeactivated)
            );
            assert!(
                !scheduler
                    .worker_authorized_for_project(WORKER_ID, projects[0])
                    .await
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn a_reauth_request_and_auth_response_cannot_bring_back_a_switched_off_capability() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(
                &db,
                worker_registration::Model {
                    enable_exec: false,
                    ..registration(projects[0], TOKEN_HASH)
                },
            )
            .await;
            let (scheduler, _session, mut worker) = open_session(&db, projects[0]).await;

            send_worker_message(&mut worker, ClientMessage::ReauthRequest).await;
            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                HashSet::from([projects[0].to_string()])
            );
            assert_eq!(
                next_server_message(&mut worker).await,
                ServerMessage::AuthChallenge {
                    peers: vec![projects[0].to_string()],
                }
            );
            send_worker_message(
                &mut worker,
                auth_response_with_valid_tokens_for(&[projects[0]]),
            )
            .await;
            next_server_message(&mut worker).await;

            let capabilities = scheduler
                .worker_gradient_caps(WORKER_ID)
                .await
                .expect("the worker is registered");
            assert!(!capabilities.exec);
            assert!(capabilities.build);
            db.finish().await;
        }

        #[tokio::test]
        async fn an_auth_response_proving_one_more_registration_keeps_its_project_on_the_next_reauth()
         {
            let (db, projects) = database_with_cached_projects(2).await;
            for project in &projects {
                insert::<worker_registration::Entity>(&db, registration(*project, TOKEN_HASH))
                    .await;
            }
            let (scheduler, session, mut worker) = open_session(&db, projects[0]).await;
            let both: HashSet<String> = projects.iter().map(ToString::to_string).collect();

            send_worker_message(&mut worker, auth_response_with_valid_tokens_for(&projects)).await;
            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                both
            );
            session
                .send_message(SessionMsg::Signal(SessionSignal::Reauth))
                .unwrap();

            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                both
            );
            assert!(
                scheduler
                    .worker_authorized_for_project(WORKER_ID, projects[1])
                    .await
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn an_auth_response_cannot_prove_a_server_dialed_registration_in_a_worker_dialed_session()
         {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, dialed_registration(projects[0])).await;
            let (scheduler, _session, mut worker) = open_session(&db, projects[0]).await;

            send_worker_message(
                &mut worker,
                auth_response_with_valid_tokens_for(&[projects[0]]),
            )
            .await;

            assert_eq!(
                next_server_message(&mut worker).await,
                ServerMessage::from(Rejection::WorkerDeactivated)
            );
            assert!(
                !scheduler
                    .worker_authorized_for_project(WORKER_ID, projects[0])
                    .await
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn a_server_dialed_session_closes_on_an_auth_response_without_an_update() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let proof = HandshakeProof::ServerDialed(dialed_session(&[], false));
            let (_scheduler, _session, mut worker) =
                open_session_with_proof(&db, projects[0], proof).await;

            send_worker_message(
                &mut worker,
                auth_response_with_valid_tokens_for(&[projects[0]]),
            )
            .await;

            assert!(matches!(
                next_server_message(&mut worker).await,
                ServerMessage::Error { code: 400, .. }
            ));
            db.finish().await;
        }

        #[tokio::test]
        async fn a_reauth_request_of_a_server_dialed_session_without_a_dial_url_rejects_it() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let proof = HandshakeProof::ServerDialed(dialed_session(&[], false));
            let (scheduler, _session, mut worker) =
                open_session_with_proof(&db, projects[0], proof).await;

            send_worker_message(&mut worker, ClientMessage::ReauthRequest).await;

            assert_eq!(
                next_server_message(&mut worker).await,
                ServerMessage::from(Rejection::WorkerDeactivated)
            );
            assert!(
                !scheduler
                    .worker_authorized_for_project(WORKER_ID, projects[0])
                    .await
            );
            db.finish().await;
        }

        async fn recheck_keeps_session(session: &ActorRef<SessionMsg>) -> bool {
            matches!(
                session
                    .call(SessionMsg::RecheckAuthorization, None)
                    .await
                    .unwrap(),
                ractor::rpc::CallResult::Success(true)
            )
        }

        async fn assert_recheck_still_authorizes(
            session: &ActorRef<SessionMsg>,
            scheduler: &Scheduler,
            project: ProjectId,
        ) {
            assert!(recheck_keeps_session(session).await);
            assert!(
                scheduler
                    .worker_authorized_for_project(WORKER_ID, project)
                    .await
            );
        }

        async fn scheduler_projects_of(
            scheduler: &Scheduler,
            projects: &[ProjectId],
        ) -> HashSet<ProjectId> {
            let mut authorized = HashSet::new();
            for project in projects {
                if scheduler
                    .worker_authorized_for_project(WORKER_ID, *project)
                    .await
                {
                    authorized.insert(*project);
                }
            }
            authorized
        }

        fn peers_of(projects: &[ProjectId]) -> HashSet<String> {
            projects.iter().map(ToString::to_string).collect()
        }

        #[tokio::test]
        async fn a_server_dialed_session_that_loses_one_of_three_projects_keeps_the_other_two() {
            let (db, projects) = database_with_cached_projects(3).await;
            for project in &projects {
                insert::<worker_registration::Entity>(&db, dialed_registration(*project)).await;
            }
            let proof = HandshakeProof::ServerDialed(dialed_session(&projects, false));
            let (scheduler, session, mut worker) =
                open_session_with_proof(&db, projects[0], proof).await;
            assert!(recheck_keeps_session(&session).await);
            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                peers_of(&projects)
            );

            worker_registration::Entity::delete_many()
                .filter(worker_registration::Column::PeerId.eq(projects[1]))
                .exec(&db.connection())
                .await
                .unwrap();
            assert!(recheck_keeps_session(&session).await);

            let rest = [projects[0], projects[2]];
            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                peers_of(&rest)
            );
            assert_eq!(
                scheduler_projects_of(&scheduler, &projects).await,
                HashSet::from(rest)
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn a_recheck_drops_only_the_project_whose_cache_was_removed() {
            let (db, projects) = database_with_cached_projects(2).await;
            for project in &projects {
                insert::<worker_registration::Entity>(&db, registration(*project, TOKEN_HASH))
                    .await;
            }
            let (scheduler, session, mut worker) =
                open_session_with_proof(&db, projects[0], worker_dialed_proof(&projects)).await;
            assert!(recheck_keeps_session(&session).await);
            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                peers_of(&projects)
            );

            EProjectCache::delete_many()
                .filter(gradient_entity::project_cache::Column::Project.eq(projects[1]))
                .exec(&db.connection())
                .await
                .unwrap();
            assert!(recheck_keeps_session(&session).await);

            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                peers_of(&projects[..1])
            );
            assert_eq!(
                scheduler_projects_of(&scheduler, &projects).await,
                HashSet::from([projects[0]])
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn an_interval_recheck_sends_an_auth_update_only_when_the_projects_change() {
            let (db, projects) = database_with_cached_projects(2).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let proof = worker_dialed_proof(&projects);
            let (_scheduler, session, mut worker) =
                open_session_with_proof(&db, projects[0], proof).await;

            assert!(recheck_keeps_session(&session).await);
            insert::<worker_registration::Entity>(&db, registration(projects[1], TOKEN_HASH)).await;
            assert!(recheck_keeps_session(&session).await);

            assert_eq!(
                authorized_peers_of(next_server_message(&mut worker).await),
                projects.iter().map(ToString::to_string).collect()
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn an_interval_recheck_with_a_failing_database_keeps_the_session_and_its_project() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let (scheduler, session, _worker) = open_session(&db, projects[0]).await;

            set_registrations_readable(&db, false).await;
            assert!(recheck_keeps_session(&session).await);
            assert!(
                scheduler
                    .worker_authorized_for_project(WORKER_ID, projects[0])
                    .await
            );
            set_registrations_readable(&db, true).await;

            assert_recheck_still_authorizes(&session, &scheduler, projects[0]).await;
            db.finish().await;
        }

        #[tokio::test]
        async fn the_third_failed_interval_recheck_in_a_row_closes_the_session() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let (_scheduler, session, _worker) = open_session(&db, projects[0]).await;

            set_registrations_readable(&db, false).await;
            assert!(recheck_keeps_session(&session).await);
            assert!(recheck_keeps_session(&session).await);
            assert!(!recheck_keeps_session(&session).await);
            set_registrations_readable(&db, true).await;
            db.finish().await;
        }

        #[tokio::test]
        async fn a_successful_interval_recheck_resets_the_count_of_failed_rechecks() {
            let (db, projects) = database_with_cached_projects(1).await;
            insert::<worker_registration::Entity>(&db, registration(projects[0], TOKEN_HASH)).await;
            let (scheduler, session, _worker) = open_session(&db, projects[0]).await;

            set_registrations_readable(&db, false).await;
            assert!(recheck_keeps_session(&session).await);
            assert!(recheck_keeps_session(&session).await);
            set_registrations_readable(&db, true).await;
            assert_recheck_still_authorizes(&session, &scheduler, projects[0]).await;
            set_registrations_readable(&db, false).await;
            assert!(recheck_keeps_session(&session).await);
            assert!(recheck_keeps_session(&session).await);
            set_registrations_readable(&db, true).await;

            assert_recheck_still_authorizes(&session, &scheduler, projects[0]).await;
            db.finish().await;
        }
    }
}
