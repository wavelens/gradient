/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::http::StatusCode;
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
use gradient_scheduler::Scheduler;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use sea_orm::{DatabaseConnection, EntityTrait, IntoActiveModel};
use serde_json::{Value, json};

const TEAM_WORKER: &str = "018f6f3a-0000-7000-8000-000000000001";
const PROJECT_WORKER: &str = "550e8400-e29b-41d4-a716-446655440099";

struct AdminArea {
    db: TestDatabase,
    server: TestServer,
    auth: String,
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

async fn admin_area(superuser: bool) -> AdminArea {
    let db = TestDatabase::create().await;
    let conn = db.connection();
    insert::<user::Entity>(
        &conn,
        user::Model {
            superuser,
            ..user()
        },
    )
    .await;
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
    let router = gradient_web::create_router_with_scheduler(
        state,
        scheduler,
        gradient_proto::SessionsHandle::new(),
    )
    .expect("router builds");
    AdminArea {
        db,
        server: TestServer::new(router),
        auth: bearer(session_id),
    }
}

impl AdminArea {
    async fn workers(&self) -> Vec<Value> {
        let res = self
            .server
            .get("/api/v1/admin/workers")
            .add_header("authorization", &self.auth)
            .await;
        res.assert_status_ok();
        res.json::<Value>()["message"]
            .as_array()
            .expect("a worker list")
            .clone()
    }

    async fn worker(&self, worker_id: &str) -> Value {
        self.workers()
            .await
            .into_iter()
            .find(|worker| worker["worker_id"] == worker_id)
            .expect("the worker is listed")
    }

    async fn drain(&self, path: &str, draining: bool) -> axum_test::TestResponse {
        self.server
            .patch(path)
            .add_header("authorization", &self.auth)
            .json(&json!({ "draining": draining }))
            .await
    }
}

#[tokio::test]
async fn the_admin_worker_list_names_owner_and_served_projects_of_registered_workers() {
    let area = admin_area(true).await;

    let team_worker = area.worker(TEAM_WORKER).await;
    assert_eq!(team_worker["team"]["name"], "platform");
    assert_eq!(team_worker["display_name"], "Team builder");
    assert_eq!(team_worker["projects"][0]["name"], "test-project");
    assert_eq!(team_worker["connected"], false);

    let project_worker = area.worker(PROJECT_WORKER).await;
    assert!(project_worker.get("team").is_none());
    assert_eq!(
        project_worker["registrations"][0]["project"]["name"],
        "test-project"
    );
    assert_eq!(project_worker["projects"][0]["name"], "test-project");
    area.db.finish().await;
}

#[tokio::test]
async fn an_admin_drain_of_an_offline_worker_is_listed_until_cancelled() {
    let area = admin_area(true).await;
    let path = format!("/api/v1/admin/workers/{PROJECT_WORKER}");

    let res = area.drain(&path, true).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"]["requested_by_admin"], true);
    assert_eq!(
        area.worker(PROJECT_WORKER).await["requested_by_admin"],
        true
    );

    area.drain(&path, false).await.assert_status_ok();
    assert_eq!(
        area.worker(PROJECT_WORKER).await["requested_by_admin"],
        false
    );
    area.db.finish().await;
}

#[tokio::test]
async fn an_unknown_worker_is_not_found_and_cannot_be_drained() {
    let area = admin_area(true).await;

    area.server
        .get("/api/v1/admin/workers/unknown")
        .add_header("authorization", &area.auth)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    area.drain("/api/v1/admin/workers/unknown", true)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    area.db.finish().await;
}

#[tokio::test]
async fn the_admin_worker_list_requires_a_superuser() {
    let area = admin_area(false).await;

    area.server
        .get("/api/v1/admin/workers")
        .add_header("authorization", &area.auth)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    area.db.finish().await;
}

#[tokio::test]
async fn a_team_admin_drains_a_team_worker_on_the_team_page() {
    let area = admin_area(true).await;

    area.drain(
        &format!("/api/v1/teams/platform/workers/{TEAM_WORKER}"),
        true,
    )
    .await
    .assert_status_ok();

    let res = area
        .server
        .get("/api/v1/teams/platform/workers")
        .add_header("authorization", &area.auth)
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>()["message"][0]["requested_by_admin"],
        true
    );
    area.db.finish().await;
}

#[tokio::test]
async fn a_project_cannot_drain_its_worker() {
    let area = admin_area(true).await;

    area.drain(
        &format!("/api/v1/projects/test-project/workers/{PROJECT_WORKER}"),
        true,
    )
    .await
    .assert_status(StatusCode::CONFLICT);
    assert_eq!(
        area.worker(PROJECT_WORKER).await["requested_by_admin"],
        false
    );
    area.db.finish().await;
}
