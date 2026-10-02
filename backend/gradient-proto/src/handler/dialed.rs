/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::ids::ProjectId;
use gradient_wire::messages::{FailedPeer, GradientCapabilities, ServerMessage};
use gradient_wire::traits::{AuthOutcome, DialerAuthority};
use tracing::{info, warn};

use super::auth::{
    aggregate_enabled_caps, filter_project_peers_without_cache, negotiate_capabilities,
};
use super::socket::{ProtoWriter, send_server_msg};
use crate::outbound::{DialTarget, Dialable, dialable};

#[derive(Clone, Debug)]
pub struct DialedSession {
    pub url: String,
    pub token_projects: Vec<String>,
    pub base_worker: bool,
}

impl DialTarget {
    pub(crate) fn session(&self) -> DialedSession {
        DialedSession {
            url: self.url.clone(),
            token_projects: self.token_projects.clone(),
            base_worker: self.base_worker,
        }
    }
}

pub(super) struct DialedWorkerAuthority {
    pub state: Arc<ServerState>,
    pub session: DialedSession,
}

#[async_trait]
impl DialerAuthority for DialedWorkerAuthority {
    async fn admit(&self, worker_id: &str) -> Result<AuthOutcome> {
        Ok(admit_dialed(&self.state, worker_id, &self.session).await?)
    }

    async fn negotiate(
        &self,
        worker_id: &str,
        client: GradientCapabilities,
    ) -> Result<GradientCapabilities> {
        let enabled = aggregate_enabled_caps(&self.state, worker_id).await;
        Ok(negotiate_capabilities(&self.state, client, enabled))
    }
}

async fn admit_dialed(
    state: &ServerState,
    worker_id: &str,
    session: &DialedSession,
) -> Result<AuthOutcome, sea_orm::DbErr> {
    Ok(match dialable(state, worker_id, &session.url).await? {
        Dialable::Projects {
            projects: current, ..
        } => {
            let accepted = current
                .into_iter()
                .filter(|p| session.token_projects.contains(p))
                .collect();

            outcome_for(state, accepted).await
        }
        Dialable::Refused(reason) => refused(reason),
    })
}

pub(super) async fn refresh_dialed_peers(
    writer: &ProtoWriter,
    state: &ServerState,
    scheduler: &Scheduler,
    worker_id: &str,
    session: &DialedSession,
) -> bool {
    let outcome = match dialable(state, worker_id, &session.url).await {
        Ok(Dialable::Projects {
            projects: current, ..
        }) => {
            let added = current.iter().find(|p| !session.token_projects.contains(p));
            if !session.base_worker
                && let Some(added) = added
            {
                info!(%worker_id, project = %added, "project not covered by this session's tokens - closing for a redial");
                return false;
            }

            outcome_for(state, current).await
        }
        Ok(Dialable::Refused(reason)) => refused(reason),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to refresh a dialed session's projects");
            return true;
        }
    };

    match outcome {
        AuthOutcome::Accept {
            authorized_peers,
            failed_peers,
        } => {
            let projects: HashSet<ProjectId> = authorized_peers
                .iter()
                .filter_map(|p| p.parse().ok())
                .collect();
            scheduler.update_authorized_peers(worker_id, projects).await;
            send_server_msg(
                writer,
                &ServerMessage::AuthUpdate {
                    authorized_peers,
                    failed_peers,
                },
            )
            .await
            .is_ok()
        }
        AuthOutcome::Reject { code, reason } => {
            info!(%worker_id, code, %reason, "dialed session has no project left - disconnecting");
            let _ = send_server_msg(writer, &ServerMessage::Reject { code, reason }).await;
            false
        }
    }
}

async fn outcome_for(state: &ServerState, projects: Vec<String>) -> AuthOutcome {
    let (authorized, demoted) = filter_project_peers_without_cache(state, projects).await;
    dialed_decision(authorized, demoted)
}

fn refused(reason: &str) -> AuthOutcome {
    AuthOutcome::Reject {
        code: 403,
        reason: reason.into(),
    }
}

fn dialed_decision(authorized: Vec<String>, demoted: Vec<FailedPeer>) -> AuthOutcome {
    match (authorized.is_empty(), demoted.is_empty()) {
        (false, _) => AuthOutcome::Accept {
            authorized_peers: authorized,
            failed_peers: demoted,
        },
        (true, false) => AuthOutcome::Reject {
            code: 495,
            reason: "project has no cache subscribed".into(),
        },
        (true, true) => refused("worker is deactivated"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::project::Model as ProjectModel;
    use gradient_entity::project_cache::{CacheSubscriptionMode, Model as ProjectCacheModel};
    use gradient_entity::{base_worker, project_base_worker, worker_registration};
    use gradient_types::ids::{CacheId, ProjectCacheId};
    use gradient_wire::session::frame::WireMessage;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::time::Duration;

    const URL: &str = "wss://w1.example/proto";

    async fn sent(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> ServerMessage {
        let bytes = rx.recv().await.expect("a frame");
        ServerMessage::decode(bytes)
            .expect("decodes")
            .into_message()
            .expect("a control message")
    }

    fn session(projects: &[ProjectId]) -> DialedSession {
        DialedSession {
            url: URL.into(),
            token_projects: projects.iter().map(ToString::to_string).collect(),
            base_worker: false,
        }
    }

    fn registration(project: ProjectId, token: Option<&str>) -> worker_registration::Model {
        worker_registration::Model {
            peer_id: project,
            worker_id: "w1".into(),
            url: Some(URL.into()),
            token_encrypted: token.map(str::to_owned),
            active: true,
            ..Default::default()
        }
    }

    fn registered(rows: Vec<worker_registration::Model>) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows])
            .append_query_results([Vec::<base_worker::Model>::new()])
    }

    fn with_cache(db: MockDatabase, project: ProjectId) -> MockDatabase {
        db.append_query_results([vec![ProjectModel {
            id: project,
            ..Default::default()
        }]])
        .append_query_results([vec![ProjectCacheModel {
            id: ProjectCacheId::now_v7(),
            project,
            cache: CacheId::now_v7(),
            mode: CacheSubscriptionMode::ReadWrite,
        }]])
    }

    #[test]
    fn a_dialed_worker_without_any_project_left_is_deactivated() {
        assert_eq!(
            dialed_decision(vec![], vec![]),
            AuthOutcome::Reject {
                code: 403,
                reason: "worker is deactivated".into(),
            }
        );
    }

    #[test]
    fn a_dialed_worker_whose_projects_lack_a_cache_is_495() {
        let demoted = vec![FailedPeer {
            peer_id: "p1".into(),
            reason: "project has no cache subscribed".into(),
        }];
        assert!(matches!(
            dialed_decision(vec![], demoted),
            AuthOutcome::Reject { code: 495, .. }
        ));
    }

    #[tokio::test]
    async fn a_dialed_worker_is_admitted_only_for_the_projects_whose_tokens_it_accepted() {
        let (sent_for, joined_later) = (ProjectId::now_v7(), ProjectId::now_v7());
        let db = with_cache(
            registered(vec![
                registration(sent_for, Some("t1")),
                registration(joined_later, Some("t2")),
            ]),
            sent_for,
        );
        let state = gradient_test_support::prelude::test_state(db.into_connection());

        assert_eq!(
            admit_dialed(&state, "w1", &session(&[sent_for]))
                .await
                .unwrap(),
            AuthOutcome::Accept {
                authorized_peers: vec![sent_for.to_string()],
                failed_peers: vec![],
            }
        );
    }

    #[tokio::test]
    async fn a_registration_without_a_stored_token_is_never_admitted() {
        let project = ProjectId::now_v7();
        let db = registered(vec![registration(project, None)]);
        let state = gradient_test_support::prelude::test_state(db.into_connection());

        assert!(matches!(
            admit_dialed(&state, "w1", &session(&[project]))
                .await
                .unwrap(),
            AuthOutcome::Reject { code: 403, .. }
        ));
    }

    #[tokio::test]
    async fn a_refresh_with_a_project_outside_the_session_tokens_closes_without_an_update() {
        let (owner, intruder) = (ProjectId::now_v7(), ProjectId::now_v7());
        let db = registered(vec![
            registration(owner, Some("t1")),
            registration(intruder, Some("garbage")),
        ]);
        let state = gradient_test_support::prelude::test_state(db.into_connection());
        let scheduler = Scheduler::new(Arc::clone(&state));
        let (writer, mut rx) = ProtoWriter::spy(Duration::from_secs(1));

        let kept =
            refresh_dialed_peers(&writer, &state, &scheduler, "w1", &session(&[owner])).await;

        assert!(!kept);
        assert!(rx.try_recv().is_err(), "no AuthUpdate may reach the worker");
    }

    #[tokio::test]
    async fn a_project_enabling_a_base_worker_joins_the_running_session() {
        let (running, enabling) = (ProjectId::now_v7(), ProjectId::now_v7());
        let enabled_by = |project| project_base_worker::Model {
            project,
            ..Default::default()
        };
        let cached = |project| ProjectCacheModel {
            id: ProjectCacheId::now_v7(),
            project,
            cache: CacheId::now_v7(),
            mode: CacheSubscriptionMode::ReadWrite,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<worker_registration::Model>::new()])
            .append_query_results([vec![base_worker::Model {
                worker_id: "w1".into(),
                url: Some(URL.into()),
                token_encrypted: Some("base".into()),
                enabled: true,
                ..Default::default()
            }]])
            .append_query_results([vec![enabled_by(running), enabled_by(enabling)]])
            .append_query_results([vec![
                ProjectModel {
                    id: running,
                    ..Default::default()
                },
                ProjectModel {
                    id: enabling,
                    ..Default::default()
                },
            ]])
            .append_query_results([vec![cached(running), cached(enabling)]]);
        let state = gradient_test_support::prelude::test_state(db.into_connection());
        let scheduler = Scheduler::new(Arc::clone(&state));
        scheduler.spawn_core(None).await.unwrap();
        let (writer, mut rx) = ProtoWriter::spy(Duration::from_secs(1));
        let base_session = DialedSession {
            base_worker: true,
            ..session(&[running])
        };

        let kept = refresh_dialed_peers(&writer, &state, &scheduler, "w1", &base_session).await;

        assert!(kept);
        assert_eq!(
            sent(&mut rx).await,
            ServerMessage::AuthUpdate {
                authorized_peers: vec![running.to_string(), enabling.to_string()],
                failed_peers: vec![],
            }
        );
    }

    #[tokio::test]
    async fn a_refresh_after_a_project_left_sends_the_remaining_projects() {
        let (stays, left) = (ProjectId::now_v7(), ProjectId::now_v7());
        let db = with_cache(registered(vec![registration(stays, Some("t1"))]), stays);
        let state = gradient_test_support::prelude::test_state(db.into_connection());
        let scheduler = Scheduler::new(Arc::clone(&state));
        scheduler.spawn_core(None).await.unwrap();
        let (writer, mut rx) = ProtoWriter::spy(Duration::from_secs(1));

        let kept =
            refresh_dialed_peers(&writer, &state, &scheduler, "w1", &session(&[stays, left])).await;

        assert!(kept);
        assert_eq!(
            sent(&mut rx).await,
            ServerMessage::AuthUpdate {
                authorized_peers: vec![stays.to_string()],
                failed_peers: vec![],
            }
        );
    }

    #[tokio::test]
    async fn a_refresh_of_a_dialed_session_with_nothing_left_rejects_it() {
        let db = registered(vec![]);
        let state = gradient_test_support::prelude::test_state(db.into_connection());
        let scheduler = Scheduler::new(Arc::clone(&state));
        let (writer, mut rx) = ProtoWriter::spy(Duration::from_secs(1));

        let kept = refresh_dialed_peers(&writer, &state, &scheduler, "w1", &session(&[])).await;

        assert!(!kept);
        assert_eq!(
            sent(&mut rx).await,
            ServerMessage::Reject {
                code: 403,
                reason: "worker is deactivated".into(),
            }
        );
    }
}
