/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::{api, ids::*};
use gradient_fixtures::mock::{all_statements, exec_ok, mock_db};
use gradient_fixtures::rows::{
    cache, cache_member, cache_view_role, project, project_member, superuser, view_role,
};
use gradient_test_support::web::{bearer, make_test_server, with_auth, with_auth_as};
use gradient_types::consts::{BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_VIEW_ID};
use gradient_types::permissions::{
    CachePermission, Permission, cache_mask_from, cache_write_mask, mask_from,
};
use sea_orm::{MockDatabase, Value as SqlValue};
use serde_json::{Value, json};

fn view_member_of_test_project(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]])
        .append_query_results([vec![view_role()]])
}

fn signed_in_owner_without_key_named_like_request() -> (MockDatabase, SessionId) {
    let session_id = SessionId::now_v7();
    let db = with_auth(mock_db(), session_id).append_query_results([Vec::<api::Model>::new()]);
    (db, session_id)
}

#[tokio::test]
async fn project_pinned_key_cannot_grant_a_permission_the_owner_lacks() {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();
    let db = db.append_query_results([vec![project()]]);
    let db = view_member_of_test_project(view_member_of_test_project(db));

    let res = make_test_server(db.into_connection())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({
            "name": "deploy",
            "permissions": ["viewProject", "deleteProject"],
            "project": "test-project",
        }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<Value>()["missing_permissions"],
        json!(["deleteProject"])
    );
}

#[tokio::test]
async fn project_pinned_key_cannot_hold_create_project() {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();
    let db = view_member_of_test_project(db.append_query_results([vec![project()]]));

    let res = make_test_server(db.into_connection())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({
            "name": "deploy",
            "permissions": ["viewProject", "createProject"],
            "project": "test-project",
        }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unpinned_key_always_gets_view_project() {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();
    let expected = mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]);
    let conn = db
        .append_query_results([vec![api::Model {
            permission: expected,
            ..Default::default()
        }]])
        .append_exec_results([exec_ok()])
        .into_connection();

    let res = make_test_server(conn.clone())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "deploy", "permissions": ["triggerEvaluation"] }))
        .await;

    res.assert_status_ok();
    let insert = all_statements(conn.into_transaction_log())
        .into_iter()
        .find(|statement| statement.sql.starts_with("INSERT INTO \"api\""))
        .expect("the key row is inserted");
    let stored = insert.values.expect("insert has values").0;
    assert!(
        stored.contains(&SqlValue::BigInt(Some(expected))),
        "{stored:?}"
    );
}

#[tokio::test]
async fn create_unpinned_api_key_with_project_and_cache_permissions_stores_both_and_shows_both_in_list()
 {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();
    let project_mask = mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]);
    let cache_mask = cache_mask_from(&[CachePermission::ViewCache, CachePermission::ReadStore]);
    let row = api::Model {
        id: ApiId::now_v7(),
        owned_by: gradient_fixtures::ids::user_id(),
        name: "deploy".into(),
        permission: project_mask,
        cache_permission: cache_mask,
        ..Default::default()
    };
    let conn = db
        .append_query_results([vec![row.clone()]])
        .append_exec_results([exec_ok()])
        .into_connection();

    let res = make_test_server(conn.clone())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({
            "name": "deploy",
            "permissions": ["triggerEvaluation"],
            "cache_permissions": ["viewCache", "readStore"],
        }))
        .await;

    res.assert_status_ok();
    let insert = all_statements(conn.into_transaction_log())
        .into_iter()
        .find(|statement| statement.sql.starts_with("INSERT INTO \"api\""))
        .expect("the key row is inserted");
    let stored = insert.values.expect("insert has values").0;
    assert!(
        stored.contains(&SqlValue::BigInt(Some(project_mask))),
        "{stored:?}"
    );
    assert!(
        stored.contains(&SqlValue::BigInt(Some(cache_mask))),
        "{stored:?}"
    );

    let list_session = SessionId::now_v7();
    let db = with_auth(mock_db(), list_session).append_query_results([vec![row]]);
    let res = make_test_server(db.into_connection())
        .get("/api/v1/user/keys")
        .add_header("authorization", bearer(list_session))
        .await;

    res.assert_status_ok();
    let listed = &res.json::<Value>()["message"][0];
    assert_eq!(
        listed["permissions"],
        json!(["viewProject", "triggerEvaluation"])
    );
    assert_eq!(
        listed["cache_permissions"],
        json!(["viewCache", "readStore"])
    );
}

#[tokio::test]
async fn api_key_pinned_to_project_with_cache_permissions_returns_400() {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();
    let db = view_member_of_test_project(db.append_query_results([vec![project()]]));

    let res = make_test_server(db.into_connection())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({
            "name": "deploy",
            "permissions": ["viewProject"],
            "cache_permissions": ["readStore"],
            "project": "test-project",
        }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unpinned_key_reach_lists_caches_with_owner_role_permissions_capped_by_key() {
    let session_id = SessionId::now_v7();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: gradient_fixtures::ids::user_id(),
        permission: mask_from(&[Permission::ViewProject]),
        cache_permission: cache_write_mask(),
        ..Default::default()
    };
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![project()]]);
    let db = view_member_of_test_project(db)
        .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]])
        .append_query_results([vec![cache()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/user/keys/{}/reach", key.id))
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>()["message"],
        json!({
            "projects": [{
                "name": "test-project",
                "display_name": "Test Project",
                "permissions": ["viewProject"],
            }],
            "caches": [{
                "name": "test-cache",
                "display_name": "Test Cache",
                "permissions": ["viewCache", "readStore"],
            }],
            "create_project": false,
        })
    );
}

#[tokio::test]
async fn key_reach_lists_projects_with_permissions_shared_by_key_and_owner() {
    let session_id = SessionId::now_v7();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: gradient_fixtures::ids::user_id(),
        permission: mask_from(&[
            Permission::ViewProject,
            Permission::TriggerEvaluation,
            Permission::CreateProject,
        ]),
        ..Default::default()
    };
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![key.clone()]])
        .append_query_results([vec![project()]]);
    let db = view_member_of_test_project(db);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/user/keys/{}/reach", key.id))
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body["message"],
        json!({
            "projects": [{
                "name": "test-project",
                "display_name": "Test Project",
                "permissions": ["viewProject"],
            }],
            "caches": [],
            "create_project": true,
        })
    );
}

#[tokio::test]
async fn key_reach_of_another_users_key_is_not_found() {
    let session_id = SessionId::now_v7();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: UserId::now_v7(),
        ..Default::default()
    };
    let db = with_auth(mock_db(), session_id).append_query_results([vec![key.clone()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/user/keys/{}/reach", key.id))
        .add_header("authorization", bearer(session_id))
        .await;

    res.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn non_superuser_create_api_key_with_superuser_rights_returns_403_and_lists_missing_superuser_rights()
 {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();

    let res = make_test_server(db.into_connection())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "admin", "permissions": ["useSuperuserRights"] }))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<Value>()["missing_permissions"],
        json!(["useSuperuserRights"])
    );
}

#[tokio::test]
async fn superuser_creates_unpinned_api_key_with_use_superuser_rights_and_view_project() {
    let session_id = SessionId::now_v7();
    let expected = mask_from(&[Permission::ViewProject, Permission::UseSuperuserRights]);
    let conn = with_auth_as(mock_db(), session_id, superuser())
        .append_query_results([Vec::<api::Model>::new()])
        .append_query_results([vec![api::Model {
            permission: expected,
            ..Default::default()
        }]])
        .append_exec_results([exec_ok()])
        .into_connection();

    let res = make_test_server(conn.clone())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({ "name": "admin", "permissions": ["useSuperuserRights"] }))
        .await;

    res.assert_status_ok();
    let insert = all_statements(conn.into_transaction_log())
        .into_iter()
        .find(|statement| statement.sql.starts_with("INSERT INTO \"api\""))
        .expect("the key row is inserted");
    let stored = insert.values.expect("insert has values").0;
    assert!(
        stored.contains(&SqlValue::BigInt(Some(expected))),
        "{stored:?}"
    );
}

#[tokio::test]
async fn api_key_pinned_to_project_cannot_have_superuser_rights() {
    let (db, session_id) = signed_in_owner_without_key_named_like_request();
    let db = view_member_of_test_project(db.append_query_results([vec![project()]]));

    let res = make_test_server(db.into_connection())
        .post("/api/v1/user/keys")
        .add_header("authorization", bearer(session_id))
        .json(&json!({
            "name": "deploy",
            "permissions": ["viewProject", "useSuperuserRights"],
            "project": "test-project",
        }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn key_permission_catalogue_hides_use_superuser_rights_from_non_superuser_and_shows_it_for_superuser()
 {
    let server_wide_permission_ids = |db: MockDatabase, session_id: SessionId| async move {
        let res = make_test_server(db.into_connection())
            .get("/api/v1/user/keys/permissions")
            .add_header("authorization", bearer(session_id))
            .await;
        res.assert_status_ok();
        res.json::<Value>()["message"]["server_wide_permissions"]
            .as_array()
            .expect("server_wide_permissions")
            .iter()
            .map(|entry| entry["id"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
    };
    let regular_session = SessionId::now_v7();
    let superuser_session = SessionId::now_v7();

    let regular =
        server_wide_permission_ids(with_auth(mock_db(), regular_session), regular_session).await;
    let admin = server_wide_permission_ids(
        with_auth_as(mock_db(), superuser_session, superuser()),
        superuser_session,
    )
    .await;

    assert_eq!(regular, vec!["createProject"]);
    assert_eq!(admin, vec!["createProject", "useSuperuserRights"]);
}
