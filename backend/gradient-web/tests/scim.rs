/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_db::users::{ValidatedNewUser, insert_user_account};
use gradient_entity::team_user::{TeamMemberSource, TeamRole};
use gradient_entity::{team, team_user, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::mock::{all_statements, empty_db, mock_db};
use gradient_test_support::state::TestState;
use gradient_types::{AccountOrigin, TeamId, TeamUserId, UserId};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, IntoActiveModel, MockExecResult,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

const SCIM_TOKEN: &str = "test-scim-token";

fn write_token() -> String {
    let path = std::env::temp_dir().join(format!("gradient-scim-token-{}", Uuid::now_v7()));
    std::fs::write(&path, SCIM_TOKEN).expect("write scim token file");
    path.to_string_lossy().into_owned()
}

fn scim_server(db: DatabaseConnection) -> TestServer {
    scim_server_with(db, false)
}

fn scim_server_with(db: DatabaseConnection, hard_delete: bool) -> TestServer {
    build_server(db, hard_delete)
}

fn build_server(db: DatabaseConnection, hard_delete: bool) -> TestServer {
    let jwt_path = std::env::temp_dir().join(format!("gradient-scim-jwt-{}", Uuid::now_v7()));
    std::fs::write(&jwt_path, "test-jwt-secret").expect("write jwt secret file");

    TestState::new()
        .web_db(db)
        .cli(|cli| {
            cli.secrets.jwt_file = jwt_path.to_string_lossy().into_owned();
            cli.scim.enable = true;
            cli.scim.token_file = Some(write_token());
            cli.scim.hard_delete = hard_delete;
        })
        .server()
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_missing_token_returns_401() {
    let db = empty_db();
    let s = scim_server(db);
    let res = s.get("/scim/v2/Users").await;
    res.assert_status_unauthorized();
    let body: Value = res.json();
    assert_eq!(
        body["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:Error"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_wrong_token_returns_401() {
    let db = empty_db();
    let s = scim_server(db);
    let res = s
        .get("/scim/v2/Users")
        .add_header("Authorization", "Bearer nope")
        .await;
    res.assert_status_unauthorized();
}

fn auth_header() -> String {
    format!("Bearer {SCIM_TOKEN}")
}

fn scim_user(username: &str, active: bool) -> user::Model {
    user::Model {
        id: Uuid::now_v7().into(),
        username: username.to_string(),
        name: username.to_string(),
        email: username.to_string(),
        managed: true,
        account_origin: AccountOrigin::ScimProvisionedByIdentityProvider,
        email_verified: true,
        active,
        ..Default::default()
    }
}

fn scim_user_with_id(id: UserId) -> user::Model {
    user::Model {
        id,
        ..scim_user("member", true)
    }
}

fn count_row(num: i64) -> BTreeMap<&'static str, sea_orm::Value> {
    let mut row = BTreeMap::new();
    row.insert("num_items", sea_orm::Value::BigInt(Some(num)));
    row
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_list_users_starts_at_the_requested_index() {
    let db = mock_db()
        .append_query_results([vec![count_row(150)]])
        .append_query_results([Vec::<user::Model>::new()])
        .into_connection();
    let server = scim_server(db.clone());
    let res = server
        .get("/scim/v2/Users")
        .add_query_param("startIndex", "51")
        .add_query_param("count", "100")
        .add_header("Authorization", auth_header())
        .await;
    res.assert_status_ok();
    drop(server);

    let page = all_statements(db.into_transaction_log())
        .into_iter()
        .find(|s| s.sql.contains("OFFSET"))
        .expect("the user list query uses an offset");
    let values = format!("{:?}", page.values);
    assert!(values.contains("(Some(50))"), "{values}");
    assert!(values.contains("(Some(100))"), "{values}");
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_create_user_returns_201() {
    let created = user::Model {
        email: "alice@example.com".into(),
        ..scim_user("alice", true)
    };
    let db = mock_db()
        .append_query_results([Vec::<user::Model>::new()])
        .append_query_results([vec![created.clone()]])
        .into_connection();
    let s = scim_server(db);
    let res = s
        .post("/scim/v2/Users")
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
            "userName": "alice",
            "emails": [{"value": "alice@example.com", "primary": true}],
            "active": true
        }))
        .await;

    res.assert_status(StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["userName"], "alice");
    assert_eq!(body["id"], created.id.to_string());
    assert_eq!(body["active"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_create_user_with_at_sign_in_username_returns_invalid_value_and_writes_nothing() {
    let db = mock_db()
        .append_query_results([Vec::<user::Model>::new()])
        .into_connection();
    let s = scim_server(db.clone());
    let res = s
        .post("/scim/v2/Users")
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
            "userName": "a@b",
            "emails": [{"value": "a@b.io", "primary": true}],
            "active": true
        }))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = res.json();
    assert_eq!(body["scimType"], "invalidValue");
    drop(s);
    let statements = all_statements(db.into_transaction_log());
    assert!(statements.iter().all(|s| !s.sql.starts_with("INSERT")));
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_get_user_returns_resource() {
    let u = scim_user("bob@example.com", true);
    let db = mock_db()
        .append_query_results([vec![u.clone()]])
        .into_connection();
    let s = scim_server(db);
    let res = s
        .get(&format!("/scim/v2/Users/{}", u.id))
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["userName"], "bob@example.com");
    assert_eq!(body["meta"]["resourceType"], "User");
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_list_users_with_username_filter() {
    let u = scim_user("carol@example.com", true);
    let db = mock_db()
        .append_query_results([vec![count_row(1)]])
        .append_query_results([vec![u.clone()]])
        .into_connection();
    let s = scim_server(db);
    let res = s
        .get("/scim/v2/Users")
        .add_query_param("filter", r#"userName eq "carol@example.com""#)
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["totalResults"], 1);
    assert_eq!(body["Resources"][0]["userName"], "carol@example.com");
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_user_active_false() {
    let u = scim_user("dave@example.com", true);
    let disabled = user::Model {
        active: false,
        ..u.clone()
    };
    let db = mock_db()
        .append_query_results([vec![u.clone()]])
        .append_query_results([vec![disabled]])
        .into_connection();
    let s = scim_server(db);
    let res = s
        .patch(&format!("/scim/v2/Users/{}", u.id))
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{"op": "replace", "path": "active", "value": false}]
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["active"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_user_with_an_unsupported_path_or_operation_is_refused() {
    for (operation, scim_type) in [
        (
            json!({"op": "replace", "path": "userName", "value": "new"}),
            "invalidPath",
        ),
        (
            json!({"op": "replace", "value": {"userName": "new"}}),
            "invalidPath",
        ),
        (json!({"op": "remove", "path": "active"}), "invalidSyntax"),
    ] {
        let u = scim_user("frank@example.com", true);
        let db = mock_db()
            .append_query_results([vec![u.clone()]])
            .into_connection();
        let res = scim_server(db)
            .patch(&format!("/scim/v2/Users/{}", u.id))
            .add_header("Authorization", auth_header())
            .json(&json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [operation]
            }))
            .await;

        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>()["scimType"], scim_type);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_database_errors_do_not_reach_the_client() {
    let db = mock_db()
        .append_query_errors([sea_orm::DbErr::Custom("relation \"user\" secret".into())])
        .into_connection();
    let res = scim_server(db)
        .get(&format!("/scim/v2/Users/{}", UserId::now_v7()))
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!res.text().contains("secret"), "{}", res.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_delete_user_soft_disables() {
    let u = scim_user("erin@example.com", true);
    // Soft delete is issuing an UPDATE (RETURNING) and never a DELETE exec. Only a query result
    // is staged, and a stray DELETE would fail with no exec staged.
    let db = mock_db()
        .append_query_results([vec![u.clone()]])
        .append_query_results([vec![user::Model {
            active: false,
            ..u.clone()
        }]])
        .into_connection();
    let s = scim_server(db);
    let res = s
        .delete(&format!("/scim/v2/Users/{}", u.id))
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_delete_user_hard_deletes() {
    let u = scim_user("frank@example.com", true);
    let db = mock_db()
        .append_query_results([vec![u.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let s = scim_server_with(db, true);
    let res = s
        .delete(&format!("/scim/v2/Users/{}", u.id))
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status(StatusCode::NO_CONTENT);
}

fn scim_team() -> team::Model {
    team::Model {
        id: TeamId::now_v7(),
        name: "acme-eng".into(),
        display_name: "ACME Eng".into(),
        scim_group: Some("acme-eng".into()),
        ..Default::default()
    }
}

fn team_member(team: TeamId, user: UserId) -> team_user::Model {
    team_user::Model {
        id: TeamUserId::now_v7(),
        team,
        user,
        role: TeamRole::Member,
        source: TeamMemberSource::ScimGroup,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_unknown_group_returns_404() {
    let db = mock_db()
        .append_query_results([Vec::<team::Model>::new()])
        .into_connection();
    let res = scim_server(db)
        .get("/scim/v2/Groups/nope")
        .add_header("Authorization", auth_header())
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_get_group_lists_the_team_members() {
    let team = scim_team();
    let member = UserId::now_v7();
    let db = mock_db()
        .append_query_results([vec![team.clone()]])
        .append_query_results([vec![team_member(team.id, member)]])
        .into_connection();
    let res = scim_server(db)
        .get("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["members"][0]["value"], member.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_group_add_member_joins_the_team() {
    let team = scim_team();
    let member = UserId::now_v7();
    let db = mock_db()
        .append_query_results([vec![team.clone()]])
        .append_query_results([vec![scim_user_with_id(member)]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![team_member(team.id, member)]])
        .into_connection();
    let res = scim_server(db)
        .patch("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{"op": "add", "path": "members", "value": [{"value": member.to_string()}]}]
        }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["members"][0]["value"], member.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_patch_group_replace_members_removes_absent_group_members() {
    let team = scim_team();
    let kept = UserId::now_v7();
    let dropped = UserId::now_v7();
    let added_by_hand = team_user::Model {
        source: TeamMemberSource::Api,
        ..team_member(team.id, UserId::now_v7())
    };
    let db = mock_db()
        .append_query_results([vec![team.clone()]])
        .append_query_results([vec![scim_user_with_id(kept)]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .append_query_results([vec![
            team_member(team.id, kept),
            team_member(team.id, dropped),
            added_by_hand.clone(),
        ]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![team_member(team.id, kept), added_by_hand]])
        .into_connection();
    let server = scim_server(db.clone());
    let res = server
        .patch("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{"op": "replace", "path": "members", "value": [{"value": kept.to_string()}]}]
        }))
        .await;
    res.assert_status_ok();
    drop(server);
    let deletes: Vec<_> = all_statements(db.into_transaction_log())
        .into_iter()
        .filter(|s| s.sql.starts_with("DELETE FROM \"team_user\""))
        .collect();
    assert_eq!(deletes.len(), 1, "only the dropped group member leaves");
    assert!(
        format!("{:?}", deletes[0].values).contains(&dropped.to_string()),
        "{:?}",
        deletes[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_scim_group_member_removal_deletes_only_members_added_by_group_and_keeps_hand_added_members()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let by_group = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    let by_hand = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "dave",
        "dave@idp.io",
    )
    .await;
    let team = insert_scim_team(&db).await;
    for member in [
        team_member(team.id, by_group.id),
        team_user::Model {
            source: TeamMemberSource::Api,
            ..team_member(team.id, by_hand.id)
        },
    ] {
        member
            .into_active_model()
            .insert(&db)
            .await
            .expect("insert member");
    }
    let server = scim_server(db.clone());

    for member in [by_group.id, by_hand.id] {
        let res = server
            .patch("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .json(&group_patch_body(json!([
                {"op": "remove", "path": format!("members[value eq \"{member}\"]")}
            ])))
            .await;
        res.assert_status_ok();
    }

    drop(server);
    let remaining = test_db.rows::<team_user::Entity>().await;
    assert_eq!(
        remaining.iter().map(|m| m.user).collect::<Vec<_>>(),
        [by_hand.id]
    );
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn inactive_user_session_returns_403() {
    let user = scim_user("mallory@example.com", false);
    let now = chrono::Utc::now().naive_utc();
    let session = gradient_entity::session::Model {
        id: gradient_types::SessionId::now_v7(),
        user_id: user.id,
        created_at: now,
        expires_at: now + chrono::Duration::hours(24),
        last_used_at: now,
        revoked_at: None,
        user_agent: None,
        ip: None,
        remember_me: false,
        is_cli_device_sign_in_session: false,
    };
    let db = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![user.clone()]])
        .into_connection();
    let s = scim_server(db);

    let claims = gradient_web::authorization::Cliams {
        iat: usize::try_from(now.and_utc().timestamp()).expect("timestamp fits"),
        exp: usize::try_from((now + chrono::Duration::hours(24)).and_utc().timestamp())
            .expect("timestamp fits"),
        id: user.id,
        jti: session.id,
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(b"test-jwt-secret").expect("jwt key"),
    )
    .expect("encode jwt");

    let res = s
        .get("/api/v1/user")
        .add_header("Authorization", format!("Bearer {token}"))
        .await;
    res.assert_status_forbidden();
}

async fn insert_account(
    db: &DatabaseConnection,
    origin: AccountOrigin,
    username: &str,
    email: &str,
) -> user::Model {
    insert_user_account(
        db,
        ValidatedNewUser::new(origin, username, username, Some(email)).expect("valid account"),
    )
    .await
    .expect("insert account")
}

async fn insert_scim_team(db: &DatabaseConnection) -> team::Model {
    team::Model {
        created_at: chrono::Utc::now().naive_utc(),
        ..scim_team()
    }
    .into_active_model()
    .insert(db)
    .await
    .expect("insert team")
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_group_add_or_replace_with_member_of_other_origin_is_refused_and_writes_nothing() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob = insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    insert_scim_team(&db).await;
    let server = scim_server(db.clone());

    for operation in [
        json!({"op": "add", "path": "members", "value": [{"value": bob.id.to_string()}]}),
        json!({"op": "replace", "path": "members", "value": [{"value": bob.id.to_string()}]}),
    ] {
        let res = server
            .patch("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .json(&json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [operation]
            }))
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
    }

    drop(server);
    assert!(test_db.rows::<team_user::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_group_reply_leaves_out_members_of_other_origin() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob = insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    let carol = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    let team = insert_scim_team(&db).await;
    for member in [
        team_user::Model {
            source: TeamMemberSource::Api,
            ..team_member(team.id, bob.id)
        },
        team_member(team.id, carol.id),
    ] {
        member
            .into_active_model()
            .insert(&db)
            .await
            .expect("insert member");
    }
    let server = scim_server(db.clone());

    let res = server
        .get("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["members"], json!([{"value": carol.id.to_string()}]));
    drop(server);
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_put_and_list_filter_do_not_reach_account_of_other_origin() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob = insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    let server = scim_server(db.clone());

    let put = server
        .put(&format!("/scim/v2/Users/{}", bob.id))
        .add_header("Authorization", auth_header())
        .json(&json!({"userName": "bob", "emails": [{"value": "evil@idp.io"}], "active": false}))
        .await;
    let list = server
        .get("/scim/v2/Users")
        .add_query_param("filter", r#"userName eq "bob""#)
        .add_header("Authorization", auth_header())
        .await;

    let user_path = format!("/scim/v2/Users/{}", bob.id);
    server
        .get(&user_path)
        .add_header("Authorization", auth_header())
        .await
        .assert_status_not_found();
    server
        .patch(&user_path)
        .add_header("Authorization", auth_header())
        .json(&json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{"op": "replace", "path": "active", "value": false}]
        }))
        .await
        .assert_status_not_found();
    server
        .delete(&user_path)
        .add_header("Authorization", auth_header())
        .await
        .assert_status_not_found();
    put.assert_status(StatusCode::NOT_FOUND);
    list.assert_status_ok();
    assert_eq!(list.json::<Value>()["totalResults"], 0);
    drop(server);
    assert_eq!(test_db.find::<user::Entity>(bob.id).await, Some(bob));
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_list_username_filter_ignores_letter_case() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "Carol",
        "carol@idp.io",
    )
    .await;
    let server = scim_server(db.clone());

    let res = server
        .get("/scim/v2/Users")
        .add_query_param("filter", r#"userName eq "carol""#)
        .add_header("Authorization", auth_header())
        .await;

    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["totalResults"], 1);
    drop(server);
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_put_with_email_of_another_account_returns_409() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    let carol = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    let server = scim_server(db.clone());

    let res = server
        .put(&format!("/scim/v2/Users/{}", carol.id))
        .add_header("Authorization", auth_header())
        .json(&json!({"userName": "carol", "emails": [{"value": "bob@self.io"}]}))
        .await;

    res.assert_status(StatusCode::CONFLICT);
    drop(server);
    test_db.finish().await;
}

fn group_patch_body(operations: Value) -> Value {
    json!({
        "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
        "Operations": operations
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_group_patch_remove_unknown_or_non_scim_account_changes_nothing_and_add_non_scim_account_fails()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob = insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    let carol = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    let team = insert_scim_team(&db).await;
    team_member(team.id, carol.id)
        .into_active_model()
        .insert(&db)
        .await
        .expect("insert member");
    let members_before = test_db.rows::<team_user::Entity>().await;
    let server = scim_server(db.clone());

    let mut answers = Vec::new();
    for id in [UserId::now_v7(), bob.id] {
        let res = server
            .patch("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .json(&group_patch_body(json!([
                {"op": "remove", "path": format!("members[value eq \"{id}\"]")}
            ])))
            .await;
        answers.push((res.status_code(), res.json::<Value>()));
    }
    let add = server
        .patch("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .json(&group_patch_body(json!([
            {"op": "add", "path": "members", "value": [{"value": bob.id.to_string()}]}
        ])))
        .await;

    assert_eq!(answers[0].0, StatusCode::OK);
    assert_eq!(answers[0], answers[1]);
    add.assert_status(StatusCode::BAD_REQUEST);
    drop(server);
    assert_eq!(test_db.rows::<team_user::Entity>().await, members_before);
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_group_patch_with_valid_add_then_other_origin_account_returns_400_and_writes_no_team_member()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob = insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    let carol = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    insert_scim_team(&db).await;
    let server = scim_server(db.clone());

    let res = server
        .patch("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .json(&group_patch_body(json!([
            {"op": "add", "path": "members", "value": [{"value": carol.id.to_string()}]},
            {"op": "add", "path": "members", "value": [{"value": bob.id.to_string()}]}
        ])))
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
    drop(server);
    assert!(test_db.rows::<team_user::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_group_patch_with_valid_add_then_database_write_failure_returns_error_and_writes_no_team_member()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let carol = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    let dave = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "dave",
        "dave@idp.io",
    )
    .await;
    insert_scim_team(&db).await;
    db.execute_unprepared(&format!(
        r#"CREATE FUNCTION raise_exception_on_team_membership_insert_for_test_user() RETURNS trigger
           LANGUAGE plpgsql AS $$ BEGIN
               IF NEW."user" = '{}' THEN RAISE division_by_zero; END IF;
               RETURN NEW;
           END $$"#,
        dave.id
    ))
    .await
    .expect("create trigger function");
    db.execute_unprepared(
        "CREATE TRIGGER raise_exception_on_team_membership_insert_for_test_user BEFORE INSERT ON team_user \
         FOR EACH ROW EXECUTE FUNCTION raise_exception_on_team_membership_insert_for_test_user()",
    )
    .await
    .expect("create trigger");
    let server = scim_server(db.clone());

    let res = server
        .patch("/scim/v2/Groups/acme-eng")
        .add_header("Authorization", auth_header())
        .json(&group_patch_body(json!([
            {"op": "add", "path": "members", "value": [{"value": carol.id.to_string()}]},
            {"op": "add", "path": "members", "value": [{"value": dave.id.to_string()}]}
        ])))
        .await;

    res.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    drop(server);
    assert!(test_db.rows::<team_user::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scim_group_patch_single_add_with_mixed_origin_members_returns_400_and_writes_no_team_member()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob = insert_account(&db, AccountOrigin::PublicRegistration, "bob", "bob@self.io").await;
    let carol = insert_account(
        &db,
        AccountOrigin::ScimProvisionedByIdentityProvider,
        "carol",
        "carol@idp.io",
    )
    .await;
    insert_scim_team(&db).await;
    let server = scim_server(db.clone());

    for members in [[carol.id, bob.id], [bob.id, carol.id]] {
        let res = server
            .patch("/scim/v2/Groups/acme-eng")
            .add_header("Authorization", auth_header())
            .json(&group_patch_body(json!([{
                "op": "add",
                "path": "members",
                "value": members.map(|id| json!({"value": id.to_string()}))
            }])))
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
    }

    drop(server);
    assert!(test_db.rows::<team_user::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_invalid_display_name_matches_put_and_is_not_stored() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let invalid = "<script>alert(1)</script>";
    let mut answers = Vec::new();
    for username in ["carol", "dave"] {
        let account = insert_account(
            &db,
            AccountOrigin::ScimProvisionedByIdentityProvider,
            username,
            &format!("{username}@idp.io"),
        )
        .await;
        let server = scim_server(db.clone());
        let path = format!("/scim/v2/Users/{}", account.id);
        let res = if username == "carol" {
            server
                .put(&path)
                .add_header("Authorization", auth_header())
                .json(&json!({"userName": username, "displayName": invalid}))
                .await
        } else {
            server
                .patch(&path)
                .add_header("Authorization", auth_header())
                .json(&group_patch_body(json!([
                    {"op": "replace", "path": "displayName", "value": invalid}
                ])))
                .await
        };
        drop(server);
        let stored = test_db
            .find::<user::Entity>(account.id)
            .await
            .expect("account exists");
        assert_eq!(stored.name, username);
        answers.push(res.status_code());
    }

    assert_eq!(answers[0], answers[1]);
    test_db.finish().await;
}
