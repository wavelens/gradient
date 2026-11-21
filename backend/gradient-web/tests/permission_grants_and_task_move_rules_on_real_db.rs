/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{TeamCacheId, TeamProjectId};
use gradient_entity::team_project::TeamGrantSource;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{
    api, cache, cache_invitation, cache_role, cache_user, commit, evaluation, pending_delivery,
    project, project_invitation, project_user, role, task, team, team_cache, team_cache_request,
    team_project, team_project_request, team_user, user,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{cache_id, commit_id, project_id};
use gradient_fixtures::rows;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::SessionId;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID,
    BASE_ROLE_WRITE_ID,
};
use gradient_types::events::Event;
use gradient_types::events::audit::Action;
use gradient_types::ids::*;
use gradient_types::permissions::{CachePermission, Permission, cache_mask_from, mask_from};
use gradient_web::authorization::hash_api_key;
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel};
use serde_json::{Value, json};
use uuid::Uuid;

struct TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader {
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

fn second_user_carol_database_row() -> user::Model {
    user::Model {
        id: UserId::now_v7(),
        username: "carol".into(),
        name: "Carol".into(),
        email: "carol@example.com".into(),
        ..rows::user()
    }
}

async fn create_test_instance_with_caller_and_carol_and_start_server() -> (
    TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    user::Model,
) {
    let db = TestDatabase::create().await;
    insert(&db, rows::user()).await;
    let carol = insert(&db, second_user_carol_database_row()).await;
    for role in [rows::admin_role(), rows::write_role(), rows::view_role()] {
        insert(&db, role).await;
    }
    let session_id = SessionId::now_v7();
    insert(&db, rows::live_session(session_id)).await;
    let crypt_file = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").expect("the key is written");
    let server = TestState::new()
        .web_db(db.connection())
        .worker_db(db.connection())
        .cli(|cli| cli.secrets.crypt_file = crypt_file.to_string_lossy().into_owned())
        .server();
    (
        TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader {
            db,
            server,
            auth: bearer(session_id),
        },
        carol,
    )
}

async fn insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    permissions: &[Permission],
) -> role::Model {
    insert(&instance.db, rows::project()).await;
    let caller_role = insert(
        &instance.db,
        role::Model {
            id: RoleId::now_v7(),
            name: "Caller".into(),
            project: Some(project_id()),
            permission: mask_from(permissions),
            managed: false,
        },
    )
    .await;
    insert(&instance.db, rows::project_member(caller_role.id)).await;
    caller_role
}

async fn insert_cache_and_role_with_permissions_and_add_calling_user(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    permissions: &[CachePermission],
) -> cache_role::Model {
    insert(&instance.db, rows::cache()).await;
    let caller_role = insert(
        &instance.db,
        cache_role::Model {
            id: RoleId::now_v7(),
            name: "Caller".into(),
            cache: Some(cache_id()),
            permission: cache_mask_from(permissions),
            managed: false,
        },
    )
    .await;
    insert(&instance.db, rows::cache_member(caller_role.id)).await;
    caller_role
}

async fn send_json_request_with_user_bearer_token(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    method: axum::http::Method,
    path: &str,
    body: Value,
) -> axum_test::TestResponse {
    instance
        .server
        .method(method, path)
        .add_header("authorization", instance.auth.clone())
        .json(&body)
        .await
}

async fn read_all_rows_of_entity<E: EntityTrait>(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> Vec<E::Model> {
    E::find()
        .all(&instance.db.connection())
        .await
        .expect("read all rows")
}

async fn read_all_project_members_and_invitations(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> (Vec<project_user::Model>, Vec<project_invitation::Model>) {
    (
        read_all_rows_of_entity::<project_user::Entity>(instance).await,
        read_all_rows_of_entity::<project_invitation::Entity>(instance).await,
    )
}

async fn read_all_cache_member_and_invitation_rows(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> (Vec<cache_user::Model>, Vec<cache_invitation::Model>) {
    (
        read_all_rows_of_entity::<cache_user::Entity>(instance).await,
        read_all_rows_of_entity::<cache_invitation::Entity>(instance).await,
    )
}

#[tokio::test]
async fn project_member_with_view_and_manage_members_role_adding_admin_user_gets_forbidden() {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[Permission::ViewProject, Permission::ManageMembers],
    )
    .await;
    let before = read_all_project_members_and_invitations(&instance).await;

    let response = send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/projects/test-project/users",
        json!({ "user": "carol", "role": "Admin", "add_as_member_immediately": true }),
    )
    .await;

    response.assert_status(StatusCode::FORBIDDEN);
    assert!(response.text().contains("deleteProject"));
    assert_eq!(
        read_all_project_members_and_invitations(&instance).await,
        before
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn test_invite_user_denied_for_admin_role_allowed_for_view_role() {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[Permission::ViewProject, Permission::ManageMembers],
    )
    .await;
    let before = read_all_project_members_and_invitations(&instance).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/projects/test-project/users",
        json!({ "user": "carol", "role": "Admin" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        read_all_project_members_and_invitations(&instance).await,
        before
    );
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/projects/test-project/users",
        json!({ "user": "carol", "role": "View" }),
    )
    .await
    .assert_status_ok();
    instance.db.finish().await;
}

#[tokio::test]
async fn member_cannot_change_role_of_or_remove_admin() {
    let (instance, carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[Permission::ViewProject, Permission::ManageMembers],
    )
    .await;
    insert(
        &instance.db,
        gradient_entity::project_user::Model {
            user: carol.id,
            ..rows::project_member(BASE_ROLE_ADMIN_ID)
        },
    )
    .await;
    let before = read_all_project_members_and_invitations(&instance).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::PATCH,
        "/api/v1/projects/test-project/users",
        json!({ "user": "carol", "role": "View" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::DELETE,
        "/api/v1/projects/test-project/users",
        json!({ "user": "carol" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        read_all_project_members_and_invitations(&instance).await,
        before
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn member_with_view_and_manage_roles_cannot_grant_permissions_they_do_not_have_to_own_or_new_role()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    let caller_role = insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[Permission::ViewProject, Permission::ManageRoles],
    )
    .await;
    let unassigned_role = insert(
        &instance.db,
        role::Model {
            id: RoleId::now_v7(),
            name: "Deleters".into(),
            project: Some(project_id()),
            permission: mask_from(&[Permission::ViewProject, Permission::DeleteProject]),
            managed: false,
        },
    )
    .await;
    let before = read_all_rows_of_entity::<role::Entity>(&instance).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/projects/test-project/roles",
        json!({ "name": "escalated", "permissions": ["viewProject", "deleteProject"] }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::PATCH,
        &format!("/api/v1/projects/test-project/roles/{}", caller_role.id),
        json!({ "permissions": ["viewProject", "manageRoles", "deleteProject"] }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::DELETE,
        &format!("/api/v1/projects/test-project/roles/{}", unassigned_role.id),
        json!({}),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        read_all_rows_of_entity::<role::Entity>(&instance).await,
        before
    );
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/projects/test-project/roles",
        json!({ "name": "readers", "permissions": ["viewProject"] }),
    )
    .await
    .assert_status_ok();
    instance.db.finish().await;
}

#[tokio::test]
async fn cache_member_with_view_cache_and_manage_cache_members_role_cannot_add_cache_admin() {
    let (instance, carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_cache_and_role_with_permissions_and_add_calling_user(
        &instance,
        &[
            CachePermission::ViewCache,
            CachePermission::ReadStore,
            CachePermission::ManageCacheMembers,
        ],
    )
    .await;
    let before = read_all_cache_member_and_invitation_rows(&instance).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/caches/test-cache/members",
        json!({ "user": "carol", "role": "Admin" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        read_all_cache_member_and_invitation_rows(&instance).await,
        before
    );
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/caches/test-cache/members",
        json!({ "user": "carol", "role": "View" }),
    )
    .await
    .assert_status_ok();

    insert(
        &instance.db,
        gradient_entity::cache_user::Model {
            user: carol.id,
            ..rows::cache_member(BASE_CACHE_ROLE_ADMIN_ID)
        },
    )
    .await;
    let before = read_all_cache_member_and_invitation_rows(&instance).await;
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::DELETE,
        "/api/v1/caches/test-cache/members",
        json!({ "user": "carol" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        read_all_cache_member_and_invitation_rows(&instance).await,
        before
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn test_cache_member_with_view_cache_and_manage_cache_roles_cannot_grant_higher_cache_role_permissions()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    let caller_role = insert_cache_and_role_with_permissions_and_add_calling_user(
        &instance,
        &[
            CachePermission::ViewCache,
            CachePermission::ManageCacheRoles,
        ],
    )
    .await;
    let before = read_all_rows_of_entity::<cache_role::Entity>(&instance).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/caches/test-cache/roles",
        json!({ "name": "writers", "permissions": ["viewCache", "writeStore"] }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::PATCH,
        &format!("/api/v1/caches/test-cache/roles/{}", caller_role.id),
        json!({ "permissions": ["viewCache", "manageCacheRoles", "writeStore"] }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        read_all_rows_of_entity::<cache_role::Entity>(&instance).await,
        before
    );
    instance.db.finish().await;
}

async fn insert_team_with_name(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    name: &str,
) -> team::Model {
    insert(
        &instance.db,
        team::Model {
            id: TeamId::now_v7(),
            name: name.into(),
            display_name: name.into(),
            ..rows::team()
        },
    )
    .await
}

async fn read_all_project_team_grants(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> Vec<team_project::Model> {
    team_project::Entity::find()
        .all(&instance.db.connection())
        .await
        .expect("Read the grants that teams have on projects.")
}

async fn read_all_team_cache_rows(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> Vec<team_cache::Model> {
    team_cache::Entity::find()
        .all(&instance.db.connection())
        .await
        .expect("Load the grants of all teams on all caches")
}

#[tokio::test]
async fn member_with_view_project_and_manage_members_cannot_grant_admin_team_role_and_gets_403_without_storing_grant_or_grant_request()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[Permission::ViewProject, Permission::ManageMembers],
    )
    .await;
    insert(&instance.db, rows::team()).await;
    insert(&instance.db, rows::team_member(TeamRole::Admin)).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/projects/test-project/teams",
        json!({ "team": "platform", "role": "Admin", "users": true }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);

    assert!(read_all_project_team_grants(&instance).await.is_empty());
    assert!(
        team_project_request::Entity::find()
            .all(&instance.db.connection())
            .await
            .expect("read all team_project_request rows")
            .is_empty()
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn member_with_only_view_and_manage_members_cannot_change_team_grant_above_own_role() {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[Permission::ViewProject, Permission::ManageMembers],
    )
    .await;
    for (name, role) in [
        ("admins", BASE_ROLE_ADMIN_ID),
        ("viewers", BASE_ROLE_VIEW_ID),
    ] {
        let team = insert_team_with_name(&instance, name).await;
        insert(
            &instance.db,
            team_project::Model {
                id: TeamProjectId::now_v7(),
                team: team.id,
                project: project_id(),
                role: Some(role),
                includes_users: true,
                ..Default::default()
            },
        )
        .await;
    }
    let before = read_all_project_team_grants(&instance).await;

    let mut http_status_codes_for_forbidden_requests = Vec::new();
    for (method, path, body) in [
        (
            axum::http::Method::PATCH,
            "/api/v1/projects/test-project/teams/admins",
            json!({ "role": "View" }),
        ),
        (
            axum::http::Method::DELETE,
            "/api/v1/projects/test-project/teams/admins",
            json!({}),
        ),
        (
            axum::http::Method::PATCH,
            "/api/v1/projects/test-project/teams/viewers",
            json!({ "role": "Admin" }),
        ),
    ] {
        http_status_codes_for_forbidden_requests.push(
            send_json_request_with_user_bearer_token(&instance, method, path, body)
                .await
                .status_code(),
        );
    }
    assert_eq!(
        http_status_codes_for_forbidden_requests,
        [StatusCode::FORBIDDEN; 3]
    );

    assert_eq!(read_all_project_team_grants(&instance).await, before);
    instance.db.finish().await;
}

#[tokio::test]
async fn cache_member_with_manage_cache_members_cannot_grant_team_admin_cache_role_and_no_grant_or_request_is_stored()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_cache_and_role_with_permissions_and_add_calling_user(
        &instance,
        &[
            CachePermission::ViewCache,
            CachePermission::ReadStore,
            CachePermission::ManageCacheMembers,
        ],
    )
    .await;
    insert(&instance.db, rows::team()).await;
    insert(&instance.db, rows::team_member(TeamRole::Admin)).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::POST,
        "/api/v1/caches/test-cache/teams",
        json!({ "team": "platform", "role": "Admin" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);

    assert!(read_all_team_cache_rows(&instance).await.is_empty());
    assert!(
        team_cache_request::Entity::find()
            .all(&instance.db.connection())
            .await
            .expect("read all team requests for a cache")
            .is_empty()
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn cache_member_with_view_cache_read_store_and_manage_members_cannot_change_team_cache_grant_beyond_own_permissions()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_cache_and_role_with_permissions_and_add_calling_user(
        &instance,
        &[
            CachePermission::ViewCache,
            CachePermission::ReadStore,
            CachePermission::ManageCacheMembers,
        ],
    )
    .await;
    for (name, role) in [
        ("admins", BASE_CACHE_ROLE_ADMIN_ID),
        ("viewers", BASE_CACHE_ROLE_VIEW_ID),
    ] {
        let team = insert_team_with_name(&instance, name).await;
        insert(
            &instance.db,
            team_cache::Model {
                id: TeamCacheId::now_v7(),
                team: team.id,
                cache: cache_id(),
                role,
                ..Default::default()
            },
        )
        .await;
    }
    let before = read_all_team_cache_rows(&instance).await;

    let mut http_status_codes_for_forbidden_requests = Vec::new();
    for (method, path, body) in [
        (
            axum::http::Method::PATCH,
            "/api/v1/caches/test-cache/teams/admins",
            json!({ "role": "View" }),
        ),
        (
            axum::http::Method::DELETE,
            "/api/v1/caches/test-cache/teams/admins",
            json!({}),
        ),
        (
            axum::http::Method::PATCH,
            "/api/v1/caches/test-cache/teams/viewers",
            json!({ "role": "Admin" }),
        ),
    ] {
        http_status_codes_for_forbidden_requests.push(
            send_json_request_with_user_bearer_token(&instance, method, path, body)
                .await
                .status_code(),
        );
    }
    assert_eq!(
        http_status_codes_for_forbidden_requests,
        [StatusCode::FORBIDDEN; 3]
    );

    assert_eq!(read_all_team_cache_rows(&instance).await, before);
    instance.db.finish().await;
}

async fn read_all_project_name_and_owner_team_id_pairs(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> Vec<(String, Option<TeamId>)> {
    project::Entity::find()
        .all(&instance.db.connection())
        .await
        .expect("projects load")
        .into_iter()
        .map(|project| (project.name, project.owner_team))
        .collect()
}

async fn read_all_cache_name_and_owner_team_id_pairs(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> Vec<(String, Option<TeamId>)> {
    cache::Entity::find()
        .all(&instance.db.connection())
        .await
        .expect("Load multiple caches")
        .into_iter()
        .map(|cache| (cache.name, cache.owner_team))
        .collect()
}

#[tokio::test]
async fn plain_team_member_setting_project_owner_team_gets_forbidden() {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert(&instance.db, rows::project()).await;
    insert(&instance.db, rows::project_member(BASE_ROLE_ADMIN_ID)).await;
    insert(&instance.db, rows::cache()).await;
    insert(&instance.db, rows::cache_member(BASE_CACHE_ROLE_ADMIN_ID)).await;
    insert(&instance.db, rows::team()).await;
    insert(&instance.db, rows::team_member(TeamRole::Member)).await;

    let mut http_status_codes_for_forbidden_requests = Vec::new();
    for (method, path, body) in [
        (
            axum::http::Method::PUT,
            "/api/v1/projects",
            json!({
                "name": "team-project", "display_name": "Team Project", "description": "",
                "owner_team": "platform",
            }),
        ),
        (
            axum::http::Method::PATCH,
            "/api/v1/projects/test-project",
            json!({ "owner_team": "platform" }),
        ),
        (
            axum::http::Method::PUT,
            "/api/v1/caches",
            json!({
                "name": "team-cache", "display_name": "Team Cache", "description": "",
                "priority": 40, "local_priority": 40, "public": false,
                "owner_team": "platform",
            }),
        ),
        (
            axum::http::Method::PATCH,
            "/api/v1/caches/test-cache",
            json!({ "owner_team": "platform" }),
        ),
    ] {
        http_status_codes_for_forbidden_requests.push(
            send_json_request_with_user_bearer_token(&instance, method, path, body)
                .await
                .status_code(),
        );
    }
    assert_eq!(
        http_status_codes_for_forbidden_requests,
        [StatusCode::FORBIDDEN; 4]
    );

    assert_eq!(
        read_all_project_name_and_owner_team_id_pairs(&instance).await,
        [("test-project".to_string(), None)]
    );
    assert_eq!(
        read_all_cache_name_and_owner_team_id_pairs(&instance).await,
        [("test-cache".to_string(), None)]
    );
    assert!(read_all_project_team_grants(&instance).await.is_empty());
    instance.db.finish().await;
}

#[tokio::test]
async fn team_admin_with_custom_project_role_gets_403_when_setting_owner_team_with_admin_default_project_role()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_project_and_caller_role_and_add_calling_user_as_member_returning_role(
        &instance,
        &[
            Permission::ViewProject,
            Permission::ManageProjectSettings,
            Permission::ManageMembers,
        ],
    )
    .await;
    insert(
        &instance.db,
        team::Model {
            new_project_users: true,
            new_project_role: Some(BASE_ROLE_ADMIN_ID),
            ..rows::team()
        },
    )
    .await;
    insert(&instance.db, rows::team_member(TeamRole::Admin)).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::PATCH,
        "/api/v1/projects/test-project",
        json!({ "owner_team": "platform" }),
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);

    assert_eq!(
        read_all_project_name_and_owner_team_id_pairs(&instance).await,
        [("test-project".to_string(), None)]
    );
    assert!(read_all_project_team_grants(&instance).await.is_empty());
    instance.db.finish().await;
}

async fn insert_platform_team_project_owned_by_platform_team_with_admin_owner_grant(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) {
    insert(&instance.db, rows::team()).await;
    insert(
        &instance.db,
        project::Model {
            owner_team: Some(rows::team().id),
            ..rows::project()
        },
    )
    .await;
    insert(
        &instance.db,
        team_project::Model {
            id: TeamProjectId::now_v7(),
            team: rows::team().id,
            project: project_id(),
            role: Some(BASE_ROLE_ADMIN_ID),
            includes_users: true,
            grant_source: TeamGrantSource::Owner,
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn project_member_with_manage_settings_and_manage_members_but_not_admin_cannot_clear_or_replace_owner_team_with_admin_grant()
 {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_platform_team_project_owned_by_platform_team_with_admin_owner_grant(&instance).await;
    let caller_role = insert(
        &instance.db,
        role::Model {
            id: RoleId::now_v7(),
            name: "Caller".into(),
            project: Some(project_id()),
            permission: mask_from(&[
                Permission::ViewProject,
                Permission::ManageProjectSettings,
                Permission::ManageMembers,
            ]),
            managed: false,
        },
    )
    .await;
    insert(&instance.db, rows::project_member(caller_role.id)).await;
    let other = insert_team_with_name(&instance, "other").await;
    insert(
        &instance.db,
        team_user::Model {
            team: other.id,
            ..rows::team_member(TeamRole::Admin)
        },
    )
    .await;
    let owners_before = read_all_project_name_and_owner_team_id_pairs(&instance).await;
    let grants_before = read_all_project_team_grants(&instance).await;

    let mut http_status_codes_for_forbidden_requests = Vec::new();
    for owner_team in [Value::Null, json!("other")] {
        http_status_codes_for_forbidden_requests.push(
            send_json_request_with_user_bearer_token(
                &instance,
                axum::http::Method::PATCH,
                "/api/v1/projects/test-project",
                json!({ "owner_team": owner_team }),
            )
            .await
            .status_code(),
        );
    }
    assert_eq!(
        http_status_codes_for_forbidden_requests,
        [StatusCode::FORBIDDEN; 2]
    );

    assert_eq!(
        read_all_project_name_and_owner_team_id_pairs(&instance).await,
        owners_before
    );
    assert_eq!(read_all_project_team_grants(&instance).await, grants_before);
    instance.db.finish().await;
}

#[tokio::test]
async fn project_admin_clears_owner_team_and_removes_owner_team_and_owner_grant() {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert_platform_team_project_owned_by_platform_team_with_admin_owner_grant(&instance).await;
    insert(&instance.db, rows::project_member(BASE_ROLE_ADMIN_ID)).await;

    send_json_request_with_user_bearer_token(
        &instance,
        axum::http::Method::PATCH,
        "/api/v1/projects/test-project",
        json!({ "owner_team": null }),
    )
    .await
    .assert_status_ok();

    assert_eq!(
        read_all_project_name_and_owner_team_id_pairs(&instance).await,
        [("test-project".to_string(), None)]
    );
    assert!(read_all_project_team_grants(&instance).await.is_empty());
    instance.db.finish().await;
}

#[tokio::test]
async fn project_admin_api_key_without_delete_project_permission_cannot_add_admin_member() {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert(&instance.db, rows::project()).await;
    insert(&instance.db, rows::project_member(BASE_ROLE_ADMIN_ID)).await;
    let raw = "k".repeat(64);
    let now = chrono::Utc::now().naive_utc();
    insert(
        &instance.db,
        api::Model {
            id: ApiId::now_v7(),
            owned_by: rows::user().id,
            name: "members-only".into(),
            key: hash_api_key(&raw),
            last_used_at: now,
            created_at: now,
            permission: mask_from(&[Permission::ViewProject, Permission::ManageMembers]),
            ..Default::default()
        },
    )
    .await;
    let before = read_all_project_members_and_invitations(&instance).await;

    let response = instance
        .server
        .post("/api/v1/projects/test-project/users")
        .add_header("authorization", format!("Bearer GRAD{raw}"))
        .json(&json!({ "user": "carol", "role": "Admin" }))
        .await;

    response.assert_status(StatusCode::FORBIDDEN);
    assert!(response.text().contains("deleteProject"));
    assert_eq!(
        read_all_project_members_and_invitations(&instance).await,
        before
    );
    instance.db.finish().await;
}

async fn create_test_instance_with_task_and_caller_member_roles_in_test_and_target_projects(
    test_project_caller_role_id: RoleId,
    calling_user_role_id_in_target_project: RoleId,
) -> (
    TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    task::Model,
    project::Model,
) {
    let (instance, _carol) = create_test_instance_with_caller_and_carol_and_start_server().await;
    insert(&instance.db, rows::project()).await;
    insert(
        &instance.db,
        rows::project_member(test_project_caller_role_id),
    )
    .await;
    let project_that_task_moves_to = insert(
        &instance.db,
        rows::project_with_id(ProjectId::now_v7(), "target-project"),
    )
    .await;
    insert(
        &instance.db,
        project_user::Model {
            id: ProjectUserId::now_v7(),
            project: project_that_task_moves_to.id,
            ..rows::project_member(calling_user_role_id_in_target_project)
        },
    )
    .await;
    let task = insert(&instance.db, rows::task()).await;
    (instance, task, project_that_task_moves_to)
}

async fn send_test_task_move_request_to_target_project(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> axum_test::TestResponse {
    send_json_request_with_user_bearer_token(
        instance,
        axum::http::Method::POST,
        "/api/v1/tasks/test-project/test-task/transfer",
        json!({ "project": "target-project" }),
    )
    .await
}

async fn read_project_id_of_task(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
    task: TaskId,
) -> ProjectId {
    task::Entity::find_by_id(task)
        .one(&instance.db.connection())
        .await
        .expect("task can be read")
        .expect("task exists")
        .project
}

async fn sorted_owner_project_ids_of_queued_task_move_audit_events(
    instance: &TestPostgresDatabaseHttpServerAndBearerAuthorizationHeader,
) -> Vec<ProjectId> {
    let mut sorted_task_move_audit_owner_project_ids: Vec<ProjectId> =
        pending_delivery::Entity::find()
            .all(&instance.db.connection())
            .await
            .expect("read pending deliveries")
            .into_iter()
            .filter_map(
                |row| match serde_json::from_value::<Event>(row.payload).ok()? {
                    Event::Audit(audited) if audited.action == Action::TaskMoveToProject => {
                        audited.owner.project
                    }
                    _ => None,
                },
            )
            .collect();
    sorted_task_move_audit_owner_project_ids.sort_by_key(ToString::to_string);
    sorted_task_move_audit_owner_project_ids
}

#[tokio::test]
async fn task_creator_with_only_view_role_cannot_move_task_to_another_project() {
    let (instance, task, _project_that_task_moves_to) =
        create_test_instance_with_task_and_caller_member_roles_in_test_and_target_projects(
            BASE_ROLE_VIEW_ID,
            BASE_ROLE_ADMIN_ID,
        )
        .await;
    assert_eq!(task.created_by, rows::user().id);

    send_test_task_move_request_to_target_project(&instance)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    assert_eq!(
        read_project_id_of_task(&instance, task.id).await,
        project_id()
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn member_with_edit_task_moves_task_and_both_projects_receive_task_move_audit_event() {
    let (instance, task, project_that_task_moves_to) =
        create_test_instance_with_task_and_caller_member_roles_in_test_and_target_projects(
            BASE_ROLE_WRITE_ID,
            BASE_ROLE_WRITE_ID,
        )
        .await;

    send_test_task_move_request_to_target_project(&instance)
        .await
        .assert_status_ok();

    assert_eq!(
        read_project_id_of_task(&instance, task.id).await,
        project_that_task_moves_to.id
    );
    let mut expected_project_ids_with_task_move_audit_event =
        vec![project_id(), project_that_task_moves_to.id];
    expected_project_ids_with_task_move_audit_event.sort_by_key(ToString::to_string);
    assert_eq!(
        sorted_owner_project_ids_of_queued_task_move_audit_events(&instance).await,
        expected_project_ids_with_task_move_audit_event
    );
    instance.db.finish().await;
}

#[tokio::test]
async fn moving_running_evaluation_task_gives_409_and_task_stays() {
    let (instance, task, _project_that_task_moves_to) =
        create_test_instance_with_task_and_caller_member_roles_in_test_and_target_projects(
            BASE_ROLE_ADMIN_ID,
            BASE_ROLE_ADMIN_ID,
        )
        .await;
    insert(
        &instance.db,
        commit::Model {
            id: commit_id(),
            hash: vec![0xab; 20],
            ..Default::default()
        },
    )
    .await;
    insert(
        &instance.db,
        evaluation::Model {
            status: EvaluationStatus::Building,
            ..rows::eval_at(EvaluationId::now_v7(), 0)
        },
    )
    .await;

    send_test_task_move_request_to_target_project(&instance)
        .await
        .assert_status(StatusCode::CONFLICT);

    assert_eq!(
        read_project_id_of_task(&instance, task.id).await,
        project_id()
    );
    assert!(
        sorted_owner_project_ids_of_queued_task_move_audit_events(&instance)
            .await
            .is_empty()
    );
    instance.db.finish().await;
}
