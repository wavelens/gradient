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
use gradient_entity::worker_registration;
use gradient_scheduler::Scheduler;
use gradient_types::EWorkerRegistration;
use gradient_types::ids::ProjectId;
use gradient_wire::messages::{FailedPeer, GradientCapabilities, ServerMessage};
use gradient_wire::traits::{AuthOutcome, DialerAuthority};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{info, warn};

use super::auth::{
    aggregate_enabled_caps, filter_project_peers_without_cache, negotiate_capabilities,
};
use super::socket::{ProtoWriter, send_server_msg};

pub(super) struct DialedWorkerAuthority {
    pub state: Arc<ServerState>,
    pub url: String,
}

#[async_trait]
impl DialerAuthority for DialedWorkerAuthority {
    async fn admit(&self, worker_id: &str) -> Result<AuthOutcome> {
        Ok(admit_dialed(&self.state, worker_id, &self.url).await)
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

pub(super) async fn admit_dialed(state: &ServerState, worker_id: &str, url: &str) -> AuthOutcome {
    let (peers, is_base) = dialed_peers(state, worker_id, url).await;
    let (authorized, demoted) = filter_project_peers_without_cache(state, peers).await;
    dialed_decision(authorized, demoted, is_base)
}

pub(super) async fn refresh_dialed_peers(
    writer: &ProtoWriter,
    state: &ServerState,
    scheduler: &Scheduler,
    worker_id: &str,
    url: &str,
) -> bool {
    match admit_dialed(state, worker_id, url).await {
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

async fn dialed_peers(state: &ServerState, worker_id: &str, url: &str) -> (Vec<String>, bool) {
    let registered = registrations_dialed_at(state, worker_id, url).await;
    if !registered.is_empty() {
        return (registered, false);
    }

    let base = gradient_db::projects::base_workers::enabled_base_worker_by_worker_id(
        &state.worker_db,
        worker_id,
    )
    .await
    .ok()
    .flatten()
    .filter(|bw| bw.url.as_deref() == Some(url));
    let Some(bw) = base else {
        return (Vec::new(), false);
    };

    let projects =
        gradient_db::projects::base_workers::projects_enabling_base_worker(&state.worker_db, bw.id)
            .await
            .unwrap_or_default();
    (projects.iter().map(ToString::to_string).collect(), true)
}

async fn registrations_dialed_at(state: &ServerState, worker_id: &str, url: &str) -> Vec<String> {
    match EWorkerRegistration::find()
        .filter(worker_registration::Column::WorkerId.eq(worker_id))
        .filter(worker_registration::Column::Url.eq(url))
        .filter(worker_registration::Column::Active.eq(true))
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows.into_iter().map(|r| r.peer_id.to_string()).collect(),
        Err(e) => {
            warn!(error = %e, %worker_id, "failed to look up dialed registrations");
            Vec::new()
        }
    }
}

fn dialed_decision(
    authorized: Vec<String>,
    demoted: Vec<FailedPeer>,
    is_base: bool,
) -> AuthOutcome {
    if !authorized.is_empty() {
        return AuthOutcome::Accept {
            authorized_peers: authorized,
            failed_peers: demoted,
        };
    }

    let (code, reason) = match (demoted.is_empty(), is_base) {
        (false, _) => (495, "project has no cache subscribed"),
        (true, true) => (403, "base worker not enabled by any project"),
        (true, false) => (403, "worker is deactivated"),
    };
    AuthOutcome::Reject {
        code,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::project::Model as ProjectModel;
    use gradient_entity::project_cache::{CacheSubscriptionMode, Model as ProjectCacheModel};
    use gradient_types::ids::{CacheId, ProjectCacheId};
    use gradient_wire::session::frame::WireMessage;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::time::Duration;

    async fn sent(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> ServerMessage {
        let bytes = rx.recv().await.expect("a frame");
        ServerMessage::decode(bytes)
            .expect("decodes")
            .into_message()
            .expect("a control message")
    }

    fn registration_with_cache(project: ProjectId) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![worker_registration::Model {
                peer_id: project,
                worker_id: "w1".into(),
                url: Some("wss://w1.example/proto".into()),
                active: true,
                ..Default::default()
            }]])
            .append_query_results([vec![ProjectModel {
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
            dialed_decision(vec![], vec![], false),
            AuthOutcome::Reject {
                code: 403,
                reason: "worker is deactivated".into(),
            }
        );
    }

    #[test]
    fn a_dialed_base_worker_without_projects_is_not_enabled() {
        assert_eq!(
            dialed_decision(vec![], vec![], true),
            AuthOutcome::Reject {
                code: 403,
                reason: "base worker not enabled by any project".into(),
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
            dialed_decision(vec![], demoted, false),
            AuthOutcome::Reject { code: 495, .. }
        ));
    }

    #[tokio::test]
    async fn a_dialed_registration_admits_the_projects_registered_at_that_url() {
        let project = ProjectId::now_v7();
        let db = registration_with_cache(project).into_connection();
        let state = gradient_test_support::prelude::test_state(db);

        assert_eq!(
            admit_dialed(&state, "w1", "wss://w1.example/proto").await,
            AuthOutcome::Accept {
                authorized_peers: vec![project.to_string()],
                failed_peers: vec![],
            }
        );
    }

    #[tokio::test]
    async fn a_reauth_of_a_dialed_session_with_nothing_left_rejects_it() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<worker_registration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::base_worker::Model>::new()])
            .into_connection();
        let state = gradient_test_support::prelude::test_state(db);
        let scheduler = Scheduler::new(Arc::clone(&state));
        let (writer, mut rx) = ProtoWriter::spy(Duration::from_secs(1));

        let kept =
            refresh_dialed_peers(&writer, &state, &scheduler, "w1", "wss://w1.example/proto").await;

        assert!(!kept);
        assert_eq!(
            sent(&mut rx).await,
            ServerMessage::Reject {
                code: 403,
                reason: "worker is deactivated".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_reauth_of_a_dialed_session_sends_its_current_projects() {
        let project = ProjectId::now_v7();
        let db = registration_with_cache(project).into_connection();
        let state = gradient_test_support::prelude::test_state(db);
        let scheduler = Scheduler::new(Arc::clone(&state));
        scheduler.spawn_core(None).await.unwrap();
        let (writer, mut rx) = ProtoWriter::spy(Duration::from_secs(1));

        let kept =
            refresh_dialed_peers(&writer, &state, &scheduler, "w1", "wss://w1.example/proto").await;

        assert!(kept);
        assert_eq!(
            sent(&mut rx).await,
            ServerMessage::AuthUpdate {
                authorized_peers: vec![project.to_string()],
                failed_peers: vec![],
            }
        );
    }
}
