/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::*;
use gradient_entity::{evaluation, project, project_user, role, session, task};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::rows::{admin_role, live_session};
use gradient_fixtures::shape::shape;
use gradient_fixtures::test_date;
use gradient_graph::Graph;
use gradient_storage::NarStore;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use sea_orm::{EntityTrait, PaginatorTrait};
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::Notify;

const LIB: &str = "cccccccccccccccccccccccccccccccc-lib.drv";
const LIB_OUT: &str = "5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f";

struct StorageArea {
    db: TestDatabase,
    run_cache_maintenance_now: Arc<Notify>,
    server: TestServer,
    auth: String,
    task_path: String,
    evaluations: [EvaluationId; 2],
    _nar_dir: TempDir,
}

async fn storage_area(superuser: bool) -> StorageArea {
    let evaluations = [EvaluationId::now_v7(), EvaluationId::now_v7()];
    let db = shape()
        .evaluation(evaluations[0])
        .evaluation_status(EvaluationStatus::Completed)
        .evaluation_option(|e| e.created_at = test_date())
        .derivation(LIB)
        .output(LIB_OUT)
        .cached_output(LIB)
        .job()
        .evaluation(evaluations[1])
        .same_task_as(evaluations[0])
        .evaluation_status(EvaluationStatus::Completed)
        .evaluation_option(|e| e.created_at = test_date() + chrono::Duration::days(2))
        .into_database()
        .await;
    db.execute(&format!(
        "UPDATE \"user\" SET superuser = {superuser}; \
         UPDATE cached_path SET file_size = 42 WHERE hash = '{LIB_OUT}'"
    ))
    .await;
    let conn = db.connection();
    let session_id = SessionId::now_v7();
    gradient_db::rows::insert::<session::Entity, _>(&conn, live_session(session_id))
        .await
        .expect("the session is written");
    gradient_db::rows::insert::<role::Entity, _>(&conn, admin_role())
        .await
        .expect("the role is written");
    let project = project::Entity::find()
        .one(&conn)
        .await
        .expect("the project reads")
        .expect("a project");
    let task = task::Entity::find()
        .one(&conn)
        .await
        .expect("the task reads")
        .expect("a task");
    gradient_db::rows::insert::<project_user::Entity, _>(
        &conn,
        project_user::Model {
            id: ProjectUserId::now_v7(),
            project: project.id,
            user: user_id(),
            role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        },
    )
    .await
    .expect("the membership is written");

    let nar_dir = TempDir::new().expect("a NAR directory");
    let graph = Graph::new();
    let state = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cache_db(conn)
        .nar_storage(NarStore::local(nar_dir.path()).expect("a local NAR store"))
        .graph(Arc::clone(&graph))
        .build();
    graph
        .spawn(state.db(), None, None)
        .await
        .expect("the graph writer starts");
    let server = TestServer::new(
        gradient_web::create_router(Arc::clone(&state)).expect("the router builds"),
    );
    StorageArea {
        db,
        run_cache_maintenance_now: Arc::clone(&state.wake.run_cache_maintenance_now),
        server,
        auth: bearer(session_id),
        task_path: format!("/api/v1/tasks/{}/{}/evaluations", project.name, task.name),
        evaluations,
        _nar_dir: nar_dir,
    }
}

impl StorageArea {
    async fn storage(&self) -> axum_test::TestResponse {
        self.server
            .get("/api/v1/admin/storage")
            .add_header("authorization", &self.auth)
            .await
    }

    async fn delete_before(&self, before: &str) -> axum_test::TestResponse {
        self.server
            .delete(&self.task_path)
            .add_query_param("before", before)
            .add_header("authorization", &self.auth)
            .await
    }

    async fn remaining_evaluations(&self) -> u64 {
        evaluation::Entity::find()
            .count(&self.db.connection())
            .await
            .expect("the evaluations count")
    }
}

#[tokio::test]
async fn the_storage_page_attributes_cached_bytes_to_task_and_project() {
    let area = storage_area(true).await;

    let res = area.storage().await;
    res.assert_status_ok();
    let storage = &res.json::<Value>()["message"];
    assert!(
        storage["disk_total_bytes"]
            .as_u64()
            .is_some_and(|total| total > 0)
    );
    assert_eq!(storage["tasks"][0]["bytes"], 42);
    assert_eq!(storage["tasks"][0]["evaluations"], 2);
    assert_eq!(storage["projects"][0]["bytes"], 42);
    assert_eq!(
        storage["projects"][0]["project"]["name"],
        storage["tasks"][0]["project"]["name"]
    );
    area.db.finish().await;
}

#[tokio::test]
async fn the_storage_page_requires_a_superuser() {
    let area = storage_area(false).await;

    area.storage().await.assert_status(StatusCode::FORBIDDEN);
    area.server
        .post("/api/v1/admin/maintenance/gc")
        .add_header("authorization", &area.auth)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    area.db.finish().await;
}

#[tokio::test]
async fn run_gc_now_wakes_the_cache_maintenance() {
    let area = storage_area(true).await;
    let wake = Arc::clone(&area.run_cache_maintenance_now);

    area.server
        .post("/api/v1/admin/maintenance/gc")
        .add_header("authorization", &area.auth)
        .await
        .assert_status(StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(1), wake.notified())
        .await
        .expect("the maintenance is woken");
    area.db.finish().await;
}

#[tokio::test]
async fn deleting_old_evaluations_keeps_the_newest_finished_one() {
    let area = storage_area(false).await;

    let res = area.delete_before("2030-01-01T00:00:00Z").await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"], 1);
    assert_eq!(area.remaining_evaluations().await, 1);
    assert!(
        evaluation::Entity::find_by_id(area.evaluations[1])
            .one(&area.db.connection())
            .await
            .expect("the evaluation reads")
            .is_some()
    );
    area.db.finish().await;
}

#[tokio::test]
async fn deleting_evaluations_needs_permission_to_edit_the_task() {
    let area = storage_area(false).await;
    area.db.execute("DELETE FROM project_user").await;

    let res = area.delete_before("2030-01-01T00:00:00Z").await;
    assert!(res.status_code().is_client_error(), "{}", res.status_code());
    assert_eq!(area.remaining_evaluations().await, 2);
    area.db.finish().await;
}
