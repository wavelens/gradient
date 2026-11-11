/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_entity::team_project::TeamGrantSource;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{task, team_project, team_worker};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use gradient_types::ids::*;
use gradient_types::{ETeamWorker, EWorkerRegistration};
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel};
use serde_json::{Value, json};

struct Instance {
    db: TestDatabase,
    server: TestServer,
    auth: String,
}

async fn insert<M: IntoActiveModel<A>, A: ActiveModelTrait + ActiveModelBehavior + Send>(
    db: &TestDatabase,
    row: M,
) -> <A::Entity as EntityTrait>::Model
where
    <A::Entity as EntityTrait>::Model: IntoActiveModel<A>,
{
    row.into_active_model()
        .insert(&db.connection())
        .await
        .expect("the row is written")
}

async fn instance_with_project_member(role: RoleId) -> Instance {
    let db = TestDatabase::create().await;
    insert(&db, rows::user()).await;
    for role in [rows::admin_role(), rows::write_role(), rows::view_role()] {
        insert(&db, role).await;
    }
    insert(&db, rows::project()).await;
    insert(&db, rows::project_member(role)).await;
    insert(&db, rows::team()).await;
    insert(&db, rows::team_member(TeamRole::Admin)).await;
    let session_id = SessionId::now_v7();
    insert(&db, rows::live_session(session_id)).await;
    let server = TestState::new().web_db(db.connection()).server();
    Instance {
        db,
        server,
        auth: bearer(session_id),
    }
}

async fn post(instance: &Instance, path: &str, body: Value) -> Value {
    let response = instance
        .server
        .post(path)
        .add_header("authorization", instance.auth.clone())
        .json(&body)
        .await;
    response.assert_status_ok();
    response.json::<Value>()["message"].clone()
}

async fn readiness(instance: &Instance) -> Value {
    let response = instance
        .server
        .get("/api/v1/projects/test-project/readiness")
        .add_header("authorization", instance.auth.clone())
        .await;
    response.assert_status_ok();
    response.json::<Value>()["message"].clone()
}

#[tokio::test]
async fn register_project_worker_returns_generated_id_token_and_nixos_snippet_and_stores_hashed_token()
 {
    let instance = instance_with_project_member(BASE_ROLE_ADMIN_ID).await;

    let registration = post(
        &instance,
        "/api/v1/projects/test-project/workers",
        json!({ "display_name": "Builder", "enable_exec": true }),
    )
    .await;

    let id = registration["id"].as_str().expect("an id").to_string();
    let token = registration["token"].as_str().expect("a token").to_string();
    assert_eq!(registration["peer_id"], rows::project().id.to_string());
    let snippet = registration["nixos_configuration"].as_str().unwrap();
    assert!(snippet.contains(&format!("id = \"{id}\";")), "{snippet}");
    assert!(snippet.contains("exec = true;"), "{snippet}");
    let stored = instance.db.rows::<EWorkerRegistration>().await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].worker_id, id);
    assert_ne!(stored[0].token_hash, token);
    assert!(password_auth::verify_password(&token, &stored[0].token_hash).is_ok());
    instance.db.finish().await;
}

#[tokio::test]
async fn register_team_worker_returns_team_id_as_peer_with_generated_id_snippet_and_stores_worker()
{
    let instance = instance_with_project_member(BASE_ROLE_ADMIN_ID).await;

    let registration = post(
        &instance,
        "/api/v1/teams/platform/workers",
        json!({ "display_name": "Team Builder" }),
    )
    .await;

    assert_eq!(registration["peer_id"], rows::team().id.to_string());
    let id = registration["id"].as_str().unwrap();
    assert!(
        registration["nixos_configuration"]
            .as_str()
            .unwrap()
            .contains(id)
    );
    let stored = instance.db.rows::<ETeamWorker>().await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].worker_id, id);
    assert_eq!(stored[0].display_name, "Team Builder");
    instance.db.finish().await;
}

#[tokio::test]
async fn test_readiness_lists_team_granted_without_workers_and_caller_team_as_grantable_not_ready()
{
    let instance = instance_with_project_member(BASE_ROLE_ADMIN_ID).await;
    insert(
        &instance.db,
        team_project::Model {
            id: TeamProjectId::now_v7(),
            team: rows::team().id,
            project: rows::project().id,
            role: Some(BASE_ROLE_VIEW_ID),
            includes_users: true,
            includes_workers: false,
            grant_source: TeamGrantSource::Manual,
            created_at: gradient_types::now(),
        },
    )
    .await;
    insert(
        &instance.db,
        team_worker::Model {
            id: TeamWorkerId::now_v7(),
            team: rows::team().id,
            worker_id: uuid::Uuid::now_v7().to_string(),
            display_name: "Team Builder".into(),
            active: true,
            enable_eval: true,
            enable_build: true,
            created_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await;

    let readiness = readiness(&instance).await;

    assert_eq!(readiness["ready"], false);
    assert_eq!(readiness["workers"]["ready"], false);
    assert_eq!(
        readiness["workers"]["teams_granted_without_workers"],
        json!([{ "name": "platform", "display_name": "Platform", "workers": 1 }])
    );
    assert_eq!(
        readiness["workers"]["teams_you_can_grant"][0]["name"],
        "platform"
    );
    assert_eq!(readiness["workers"]["teams"], json!([]));
    assert_eq!(readiness["cache"]["ready"], false);
    assert_eq!(readiness["repository"]["status"], "unchecked");
    instance.db.finish().await;
}

#[tokio::test]
async fn test_readiness_hides_repository_fetch_error_from_view_only_member_and_shows_failing_status()
 {
    let instance = instance_with_project_member(BASE_ROLE_VIEW_ID).await;
    insert(
        &instance.db,
        task::Model {
            last_fetch_at: Some(gradient_types::now()),
            last_fetch_error: Some("ssh: git.internal.example: permission denied".into()),
            ..rows::task()
        },
    )
    .await;

    let readiness = readiness(&instance).await;

    assert_eq!(readiness["repository"]["status"], "failing");
    assert_eq!(readiness["repository"]["tasks"][0]["status"], "failing");
    assert!(readiness["repository"]["tasks"][0].get("error").is_none());
    assert_eq!(readiness["workers"]["teams_you_can_grant"], json!([]));
    instance.db.finish().await;
}
