/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{role, team_worker, worker_registration};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::consts::BASE_ROLE_VIEW_ID;
use gradient_types::ids::{RoleId, TeamWorkerId};
use gradient_types::permissions::{Permission, mask_from};
use gradient_types::{
    EAuditLog, EProject, EProjectUser, ERole, ESession, ETeam, ETeamUser, ETeamWorker, EUser,
    EWorkerRegistration, SessionId,
};
use sea_orm::EntityTrait;
use serde_json::json;

const URL: &str = "/api/v1/gradient-ci/connections";
const WORKER_ID: &str = "0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b";
const TOKEN: &str = "gci1_0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b_s3cret";

struct Instance {
    db: TestDatabase,
    server: TestServer,
    auth: String,
}

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

fn temp_crypt_file() -> String {
    let path = std::env::temp_dir().join(format!("gradient-test-crypt-{}", uuid::Uuid::now_v7()));
    std::fs::write(&path, "this-is-a-32-byte-crypt-key!!!!").expect("write temp secret");
    path.to_string_lossy().into_owned()
}

async fn signed_in_instance(db: TestDatabase) -> Instance {
    insert::<EUser>(&db, rows::user()).await;
    let session_id = SessionId::now_v7();
    insert::<ESession>(&db, rows::live_session(session_id)).await;
    let server = TestState::new()
        .web_db(db.connection())
        .worker_db(db.connection())
        .cli(|cli| cli.secrets.crypt_file = temp_crypt_file())
        .server();
    Instance {
        db,
        server,
        auth: bearer(session_id),
    }
}

async fn project_member_with(permissions: Option<&[Permission]>) -> Instance {
    let instance = signed_in_instance(TestDatabase::create().await).await;
    let db = &instance.db;
    insert::<EProject>(db, rows::project()).await;
    let role = match permissions {
        None => {
            insert::<ERole>(db, rows::view_role()).await;
            BASE_ROLE_VIEW_ID
        }
        Some(permissions) => {
            let custom = role::Model {
                id: RoleId::now_v7(),
                name: "Workers".into(),
                project: Some(rows::project().id),
                permission: mask_from(permissions),
                managed: false,
            };
            insert::<ERole>(db, custom.clone()).await;
            custom.id
        }
    };
    insert::<EProjectUser>(db, rows::project_member(role)).await;
    instance
}

async fn audit_events(db: &TestDatabase) -> Vec<String> {
    db.rows::<EAuditLog>()
        .await
        .into_iter()
        .map(|row| row.event)
        .collect()
}

#[tokio::test]
async fn test_view_role_project_member_gets_403_when_creating_changing_or_deleting_project_gradient_ci_connection()
 {
    let instance = project_member_with(None).await;
    insert::<EWorkerRegistration>(
        &instance.db,
        worker_registration::Model {
            peer_id: rows::project().id,
            worker_id: WORKER_ID.into(),
            url: Some("wss://servers.gradient.ci/proto".into()),
            gradient_ci: true,
            created_by: None,
            ..rows::worker_registration(true, true)
        },
    )
    .await;
    let worker = format!("/api/v1/projects/test-project/workers/{WORKER_ID}");

    instance
        .server
        .post(URL)
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "scope": "project", "project": "test-project", "token": TOKEN }))
        .await
        .assert_status_forbidden();
    instance
        .server
        .patch(&worker)
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "active": false }))
        .await
        .assert_status_forbidden();
    instance
        .server
        .delete(&worker)
        .add_header("authorization", instance.auth.clone())
        .await
        .assert_status_forbidden();

    let registrations = instance.db.rows::<EWorkerRegistration>().await;
    assert_eq!(registrations.len(), 1);
    assert!(registrations[0].active);
    instance.db.finish().await;
}

#[tokio::test]
async fn test_project_member_with_manage_workers_connects_project_to_gradient_ci_and_writes_project_worker_create_audit_log_row()
 {
    let instance =
        project_member_with(Some(&[Permission::ViewProject, Permission::ManageWorkers])).await;

    instance
        .server
        .post(URL)
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "scope": "project", "project": "test-project", "token": TOKEN }))
        .await
        .assert_status_ok();

    let registrations = instance.db.rows::<EWorkerRegistration>().await;
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].worker_id, WORKER_ID);
    assert!(registrations[0].gradient_ci && registrations[0].url.is_some());
    assert_eq!(audit_events(&instance.db).await, ["project.worker.create"]);
    instance.db.finish().await;
}

#[tokio::test]
async fn test_team_member_without_admin_role_gets_403_when_creating_changing_or_deleting_team_gradient_ci_connection()
 {
    let instance = signed_in_instance(TestDatabase::create().await).await;
    insert::<ETeam>(&instance.db, rows::team()).await;
    insert::<ETeamUser>(&instance.db, rows::team_member(TeamRole::Member)).await;
    insert::<ETeamWorker>(
        &instance.db,
        team_worker::Model {
            id: TeamWorkerId::now_v7(),
            team: rows::team().id,
            worker_id: WORKER_ID.into(),
            url: Some("wss://servers.gradient.ci/proto".into()),
            gradient_ci: true,
            enable_eval: true,
            enable_build: true,
            active: true,
            created_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await;
    let worker = format!("/api/v1/teams/platform/workers/{WORKER_ID}");

    instance
        .server
        .post(URL)
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "scope": "team", "team": "platform", "token": TOKEN }))
        .await
        .assert_status_forbidden();
    instance
        .server
        .patch(&worker)
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "active": false }))
        .await
        .assert_status_forbidden();
    instance
        .server
        .delete(&worker)
        .add_header("authorization", instance.auth.clone())
        .await
        .assert_status_forbidden();

    let workers = instance.db.rows::<ETeamWorker>().await;
    assert_eq!(workers.len(), 1);
    assert!(workers[0].active);
    assert!(audit_events(&instance.db).await.is_empty());
    instance.db.finish().await;
}

#[tokio::test]
async fn test_team_admin_connects_team_to_gradient_ci_and_writes_team_worker_create_audit_log_row()
{
    let instance = signed_in_instance(TestDatabase::create().await).await;
    insert::<ETeam>(&instance.db, rows::team()).await;
    insert::<ETeamUser>(&instance.db, rows::team_member(TeamRole::Admin)).await;

    instance
        .server
        .post(URL)
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "scope": "team", "team": "platform", "token": TOKEN }))
        .await
        .assert_status_ok();

    let workers = instance.db.rows::<ETeamWorker>().await;
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].worker_id, WORKER_ID);
    assert_eq!(audit_events(&instance.db).await, ["team.worker.create"]);
    instance.db.finish().await;
}
