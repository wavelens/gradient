/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum_test::TestServer;
use gradient_entity::ids::*;
use gradient_entity::{
    project, project_user, role, session, team, team_project, team_worker, user,
    worker_registration,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{project_id, team_id, user_id};
use gradient_fixtures::rows::{admin_role, live_session, project, team, user};
use gradient_fixtures::test_date;
use gradient_pool::ProjectAccess;
use gradient_pool::session_port::SessionSignal;
use gradient_scheduler::Scheduler;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_wire::types::GradientCapabilities;
use sea_orm::{DatabaseConnection, EntityTrait, IntoActiveModel};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

const TEAM_WORKER: &str = "018f6f3a-0000-7000-8000-000000000001";
const PROJECT_WORKER: &str = "550e8400-e29b-41d4-a716-446655440099";

struct ConnectedProjectAndTeamWorkerSignalTestFixture {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    project_worker_signals: UnboundedReceiver<SessionSignal>,
    team_worker_signals: UnboundedReceiver<SessionSignal>,
}

async fn insert<E>(db: &DatabaseConnection, row: E::Model)
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: sea_orm::ActiveModelBehavior + Send,
{
    gradient_db::rows::insert::<E, _>(db, row)
        .await
        .expect("the row is written");
}

async fn register_test_project_allowed_worker_and_return_session_signal_receiver(
    scheduler: &Scheduler,
    worker_id: &str,
) -> UnboundedReceiver<SessionSignal> {
    let (port, signals) = tokio::sync::mpsc::unbounded_channel();
    scheduler
        .register_worker(
            worker_id,
            GradientCapabilities::default(),
            ProjectAccess::OnlyListedProjects(HashSet::from([project_id()])),
            Arc::new(port),
            false,
        )
        .await
        .expect("the worker registers");
    signals
}

async fn connected_workers() -> ConnectedProjectAndTeamWorkerSignalTestFixture {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    insert::<user::Entity>(&conn, user()).await;
    let session_id = SessionId::now_v7();
    insert::<session::Entity>(&conn, live_session(session_id)).await;
    insert::<team::Entity>(&conn, team()).await;
    insert::<project::Entity>(&conn, project()).await;
    insert::<role::Entity>(&conn, admin_role()).await;
    insert::<project_user::Entity>(
        &conn,
        project_user::Model {
            id: ProjectUserId::now_v7(),
            project: project_id(),
            user: user_id(),
            role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        },
    )
    .await;
    insert::<team_project::Entity>(
        &conn,
        team_project::Model {
            id: TeamProjectId::now_v7(),
            team: team_id(),
            project: project_id(),
            role: None,
            includes_users: false,
            includes_workers: true,
            grant_source: gradient_types::TeamGrantSource::Manual,
            created_at: test_date(),
        },
    )
    .await;
    insert::<team_worker::Entity>(
        &conn,
        team_worker::Model {
            id: TeamWorkerId::now_v7(),
            team: team_id(),
            worker_id: TEAM_WORKER.into(),
            token_hash: "hash".into(),
            display_name: "Team builder".into(),
            active: true,
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await;
    insert::<worker_registration::Entity>(
        &conn,
        worker_registration::Model {
            id: WorkerRegistrationId::now_v7(),
            peer_id: project_id(),
            worker_id: PROJECT_WORKER.into(),
            token_hash: "hash".into(),
            display_name: "Project builder".into(),
            active: true,
            enable_exec: true,
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await;

    let state = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cache_db(conn)
        .build();
    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    scheduler.spawn_core(None).await.expect("scheduler core");
    let project_worker_signals =
        register_test_project_allowed_worker_and_return_session_signal_receiver(
            &scheduler,
            PROJECT_WORKER,
        )
        .await;
    let team_worker_signals =
        register_test_project_allowed_worker_and_return_session_signal_receiver(
            &scheduler,
            TEAM_WORKER,
        )
        .await;
    let router = gradient_web::create_router_with_scheduler(
        state,
        scheduler,
        gradient_proto::SessionsHandle::new(),
    )
    .expect("router builds");
    ConnectedProjectAndTeamWorkerSignalTestFixture {
        db,
        server: TestServer::new(router),
        auth: bearer(session_id),
        project_worker_signals,
        team_worker_signals,
    }
}

async fn wait_up_to_five_seconds_for_reauthentication_signal_or_panic(
    signals: &mut UnboundedReceiver<SessionSignal>,
    worker: &str,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(signal) = signals.recv().await {
            if signal == SessionSignal::Reauth {
                return;
            }
        }
        panic!("The session for {worker} closed without a reauth signal");
    })
    .await
    .unwrap_or_else(|_| panic!("The {worker} session did not receive the reauth signal."));
}

#[tokio::test]
async fn deleting_project_sends_reauthentication_signal_to_project_worker_and_team_worker() {
    let mut workers = connected_workers().await;

    workers
        .server
        .delete("/api/v1/projects/test-project")
        .add_header("authorization", &workers.auth)
        .await
        .assert_status_ok();

    wait_up_to_five_seconds_for_reauthentication_signal_or_panic(
        &mut workers.project_worker_signals,
        PROJECT_WORKER,
    )
    .await;
    wait_up_to_five_seconds_for_reauthentication_signal_or_panic(
        &mut workers.team_worker_signals,
        TEAM_WORKER,
    )
    .await;
    workers.db.finish().await;
}

#[tokio::test]
async fn disabling_exec_on_project_worker_signals_session_reauthentication() {
    let mut workers = connected_workers().await;

    workers
        .server
        .patch(&format!(
            "/api/v1/projects/test-project/workers/{PROJECT_WORKER}"
        ))
        .add_header("authorization", &workers.auth)
        .json(&json!({ "enable_exec": false }))
        .await
        .assert_status_ok();

    wait_up_to_five_seconds_for_reauthentication_signal_or_panic(
        &mut workers.project_worker_signals,
        PROJECT_WORKER,
    )
    .await;
    workers.db.finish().await;
}
