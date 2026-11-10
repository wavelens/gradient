/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{project, project_user, team, team_user, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::SessionId;
use gradient_types::consts::{BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::ids::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct Instance {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    team: team::Model,
    carol: user::Model,
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

async fn add_to_team(db: &TestDatabase, team: TeamId, user: UserId, role: TeamRole) {
    insert(
        db,
        team_user::Model {
            id: TeamUserId::now_v7(),
            team,
            user,
            role,
            ..Default::default()
        },
    )
    .await;
}

async fn sign_in_user_in_real_test_database_and_return_authorization_header(
    db: &TestDatabase,
) -> String {
    let session_id = SessionId::now_v7();
    insert(db, rows::live_session(session_id)).await;
    bearer(session_id)
}

async fn instance_with_team_writing_on_its_projects(caller_in_team: bool) -> Instance {
    let db = TestDatabase::create().await;
    insert(&db, rows::user()).await;
    let carol = insert(
        &db,
        user::Model {
            id: UserId::now_v7(),
            username: "carol".into(),
            name: "Carol".into(),
            email: "carol@example.com".into(),
            ..rows::user()
        },
    )
    .await;
    for role in [rows::admin_role(), rows::write_role(), rows::view_role()] {
        insert(&db, role).await;
    }
    let team = insert(
        &db,
        team::Model {
            new_project_users: true,
            new_project_role: Some(BASE_ROLE_WRITE_ID),
            ..rows::team()
        },
    )
    .await;
    add_to_team(&db, team.id, carol.id, TeamRole::Member).await;
    if caller_in_team {
        add_to_team(&db, team.id, rows::user().id, TeamRole::Admin).await;
    }
    let auth = sign_in_user_in_real_test_database_and_return_authorization_header(&db).await;
    let crypt_file = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").expect("the key is written");
    let server = TestState::new()
        .web_db(db.connection())
        .cli(|cli| cli.secrets.crypt_file = crypt_file.to_string_lossy().into_owned())
        .server();
    Instance {
        db,
        server,
        auth,
        team,
        carol,
    }
}

async fn create_team_project(instance: &Instance) -> axum_test::TestResponse {
    instance
        .server
        .put("/api/v1/projects")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({
            "name": "team-project",
            "display_name": "Team Project",
            "description": "",
            "owner_team": instance.team.name,
        }))
        .await
}

async fn role_of_carol(instance: &Instance) -> Value {
    let res = instance
        .server
        .get("/api/v1/projects/team-project/access")
        .add_header("authorization", instance.auth.clone())
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    body["message"]
        .as_array()
        .expect("one row per person")
        .iter()
        .find(|row| row["user"] == "carol")
        .map(|row| row["role"].clone())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn create_project_with_owner_team_caller_not_member_is_forbidden_and_writes_no_project() {
    let instance = instance_with_team_writing_on_its_projects(false).await;

    create_team_project(&instance)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    assert!(instance.db.rows::<project::Entity>().await.is_empty());
    instance.db.finish().await;
}

#[tokio::test]
async fn direct_project_membership_role_overrides_team_default_role_for_team_owned_project() {
    let instance = instance_with_team_writing_on_its_projects(true).await;
    create_team_project(&instance).await.assert_status_ok();
    assert_eq!(role_of_carol(&instance).await, "Write");

    let res = instance
        .server
        .post("/api/v1/projects/team-project/users")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "user": "carol", "role": "View", "add_as_member_immediately": true }))
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"], "User added");

    assert_eq!(role_of_carol(&instance).await, "View");
    instance.db.finish().await;
}

#[tokio::test]
async fn test_instance_admin_revoke_access_removes_project_and_team_memberships_and_rejects_non_admin()
 {
    let instance = instance_with_team_writing_on_its_projects(false).await;
    let project = insert(&instance.db, rows::project()).await;
    insert(
        &instance.db,
        project_user::Model {
            id: ProjectUserId::now_v7(),
            project: project.id,
            user: instance.carol.id,
            role: BASE_ROLE_VIEW_ID,
        },
    )
    .await;
    let revoke = || {
        instance
            .server
            .post("/api/v1/users/carol/revoke-access")
            .add_header("authorization", instance.auth.clone())
    };

    revoke().await.assert_status(StatusCode::FORBIDDEN);
    let mut caller = user::Entity::find_by_id(rows::user().id)
        .one(&instance.db.connection())
        .await
        .expect("the caller is read")
        .expect("the caller exists")
        .into_active_model();
    caller.superuser = Set(true);
    caller
        .update(&instance.db.connection())
        .await
        .expect("the caller becomes an instance admin");
    revoke().await.assert_status_ok();

    let remaining_project_rows = project_user::Entity::find()
        .filter(project_user::Column::User.eq(instance.carol.id))
        .all(&instance.db.connection())
        .await
        .expect("the project rows are read");
    let remaining_team_rows = team_user::Entity::find()
        .filter(team_user::Column::User.eq(instance.carol.id))
        .all(&instance.db.connection())
        .await
        .expect("the team rows are read");
    assert!(remaining_project_rows.is_empty());
    assert!(remaining_team_rows.is_empty());
    instance.db.finish().await;
}
