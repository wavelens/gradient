/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::extract::connect_info::MockConnectInfo;
use axum::http::Method;
use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::{audit_log, cli_device_authorization, session};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{self, live_session, user};
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_test_support::web::bearer;
use gradient_types::events::audit::Action;
use gradient_types::{CliDeviceAuthorizationId, SessionId, UserId};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::ActiveValue::Set;
use sea_orm::{MockDatabase, MockExecResult};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use uuid::Uuid;

#[derive(Serialize)]
struct Claims {
    exp: i64,
    iat: i64,
    id: UserId,
    jti: SessionId,
}

fn sign_session_jwt(user_id: UserId, session_id: SessionId) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp(),
        exp: (now + Duration::hours(1)).timestamp(),
        id: user_id,
        jti: session_id,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("sign jwt")
}

fn hash_device_code(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

fn auth_queue(db: MockDatabase, session: session::Model) -> MockDatabase {
    db.append_query_results([vec![session.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

fn server_with(web_db_setup: impl FnOnce(MockDatabase) -> MockDatabase) -> TestServer {
    TestState::new()
        .web_db(web_db_setup(mock_db()).into_connection())
        .server()
}

fn pending_row(device_code: &str, user_code: &str) -> cli_device_authorization::Model {
    let now = Utc::now().naive_utc();
    cli_device_authorization::Model {
        id: CliDeviceAuthorizationId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000c1").unwrap(),
        ),
        device_code_hash: hash_device_code(device_code),
        user_code: user_code.to_string(),
        created_at: now,
        expires_at: now + Duration::minutes(10),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn start_returns_user_code_and_verification_uri() {
    let inserted = pending_row("dev-code", "ABCD-EFGH");
    let s = server_with(|db| {
        db.append_query_results([Vec::<cli_device_authorization::Model>::new()])
            .append_query_results([vec![inserted]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
    });

    let res = s.post("/api/v1/auth/cli/start").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], Value::Bool(false));
    let m = &body["message"];
    assert!(m["device_code"].as_str().unwrap().len() >= 32);
    assert!(m["user_code"].as_str().unwrap().contains('-'));
    assert!(
        m["verification_uri"]
            .as_str()
            .unwrap()
            .ends_with("/account/cli-authorize")
    );
    assert_eq!(m["verification_uri_complete"], m["verification_uri"]);
    assert!(m["interval"].as_u64().unwrap() >= 1);
    assert!(m["expires_in"].as_i64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn start_response_carries_no_code_in_any_url() {
    let test_db = TestDatabase::create().await;
    let server = TestState::new().web_db(test_db.connection()).server();

    let res = server.post("/api/v1/auth/cli/start").await;
    res.assert_status_ok();
    let body: Value = res.json();
    let answer = body["message"].as_object().unwrap();
    let user_code = answer["user_code"].as_str().unwrap();
    for (field, value) in answer.iter().filter(|(field, _)| *field != "user_code") {
        assert!(
            !value.to_string().contains(user_code),
            "{field} carries the user code: {value}"
        );
    }
    test_db.finish().await;
}

fn server_behind_trusted_proxy(test_db: &TestDatabase) -> TestServer {
    let router = gradient_web::create_router(TestState::new().web_db(test_db.connection()).build())
        .unwrap()
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    TestServer::new(router)
}

#[tokio::test(flavor = "multi_thread")]
async fn info_reports_whether_cli_and_browser_share_a_network() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let browser_session = live_session(SessionId::now_v7());
    gradient_db::rows::insert::<session::Entity, _>(&db, browser_session.clone())
        .await
        .unwrap();
    let server = server_behind_trusted_proxy(&test_db);

    for (cli_ip, browser_ip, same_network) in [
        ("203.0.113.7", "203.0.113.7", true),
        ("203.0.113.7", "198.51.100.9", false),
        ("2001:db8:1:2::10", "2001:db8:1:2:ffff::20", true),
        ("2001:db8:1:2::10", "2001:db8:1:3::10", false),
    ] {
        let start = server
            .post("/api/v1/auth/cli/start")
            .add_header("x-forwarded-for", cli_ip)
            .await;
        start.assert_status_ok();
        let user_code = start.json::<Value>()["message"]["user_code"]
            .as_str()
            .unwrap()
            .to_owned();

        let info = server
            .get(&format!("/api/v1/auth/cli/info?user_code={user_code}"))
            .add_header("authorization", bearer(browser_session.id))
            .add_header("x-forwarded-for", browser_ip)
            .await;
        info.assert_status_ok();
        assert_eq!(
            info.json::<Value>()["message"]["cli_and_browser_on_same_network"],
            Value::Bool(same_network),
            "CLI {cli_ip}, browser {browser_ip}"
        );
        if cli_ip != browser_ip {
            assert!(!info.text().contains(browser_ip), "{}", info.text());
        }
    }
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_pending_returns_cli_auth_pending() {
    let row = pending_row("dev-code-xyz", "ABCD-EFGH");
    let s = server_with(|db| db.append_query_results([vec![row]]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["code"], "cli_auth_pending");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_denied_returns_cli_auth_denied() {
    let mut row = pending_row("dev-code-xyz", "ABCD-EFGH");
    row.denied_at = Some(Utc::now().naive_utc());
    let s = server_with(|db| db.append_query_results([vec![row]]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["code"], "cli_auth_denied");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_expired_returns_cli_auth_expired() {
    let mut row = pending_row("dev-code-xyz", "ABCD-EFGH");
    row.expires_at = Utc::now().naive_utc() - Duration::seconds(1);
    let s = server_with(|db| db.append_query_results([vec![row]]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["code"], "cli_auth_expired");
}

async fn insert_user_and_approve_pending_cli_device_authorization_with_browser_session(
    test_db: &TestDatabase,
) -> (TestServer, session::Model) {
    let db = test_db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let browser_session = live_session(SessionId::now_v7());
    gradient_db::rows::insert::<session::Entity, _>(&db, browser_session.clone())
        .await
        .unwrap();
    gradient_db::rows::insert::<cli_device_authorization::Entity, _>(
        &db,
        pending_row("dev-code-xyz", "ABCD-EFGH"),
    )
    .await
    .unwrap();
    let server = TestState::new().web_db(db).server();

    server
        .post("/api/v1/auth/cli/authorize")
        .add_header("authorization", bearer(browser_session.id))
        .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
        .await
        .assert_status_ok();
    (server, browser_session)
}

async fn poll(server: &TestServer) -> axum_test::TestResponse {
    server
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_after_approval_creates_cli_device_session_and_writes_login_success_audit() {
    let test_db = TestDatabase::create().await;
    let (server, browser_session) =
        insert_user_and_approve_pending_cli_device_authorization_with_browser_session(&test_db)
            .await;
    let approved = test_db.rows::<cli_device_authorization::Entity>().await;
    assert_eq!(approved[0].approving_session_id, Some(browser_session.id));
    assert_eq!(test_db.rows::<session::Entity>().await.len(), 1);

    let first = poll(&server).await;
    first.assert_status_ok();
    let second = poll(&server).await;
    second.assert_status_not_found();

    let sessions = test_db.rows::<session::Entity>().await;
    let cli_session = sessions
        .iter()
        .find(|s| s.id != browser_session.id)
        .unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(cli_session.is_cli_device_sign_in_session);
    assert!(cli_session.expires_at <= cli_session.created_at + Duration::days(30));
    assert!(
        test_db
            .rows::<cli_device_authorization::Entity>()
            .await
            .is_empty()
    );
    let token = first.json::<Value>()["message"]
        .as_str()
        .unwrap()
        .to_owned();
    server
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {token}"))
        .await
        .assert_status_ok();
    let login_success = test_db
        .rows::<audit_log::Entity>()
        .await
        .into_iter()
        .find(|row| row.event == Action::LoginSuccess.name())
        .unwrap();
    assert_eq!(login_success.user_id, Some(user_id()));
    assert_eq!(
        login_success.metadata,
        Some(serde_json::json!({ "method": "cli_device", "session_id": cli_session.id }))
    );
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_after_expiry_of_approved_code_returns_expired_and_creates_no_session() {
    let test_db = TestDatabase::create().await;
    let (server, _) =
        insert_user_and_approve_pending_cli_device_authorization_with_browser_session(&test_db)
            .await;
    let approved = test_db.rows::<cli_device_authorization::Entity>().await;
    let mut expired: cli_device_authorization::ActiveModel = approved[0].clone().into();
    expired.expires_at = Set(Utc::now().naive_utc() - Duration::seconds(1));
    gradient_db::rows::update::<cli_device_authorization::Entity, _>(
        &test_db.connection(),
        expired,
    )
    .await
    .unwrap();

    let res = poll(&server).await;

    res.assert_status_bad_request();
    assert_eq!(res.json::<Value>()["code"], "cli_auth_expired");
    assert_eq!(test_db.rows::<session::Entity>().await.len(), 1);
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_polls_with_same_approved_device_code_create_exactly_one_cli_device_session() {
    let test_db = TestDatabase::create().await;
    let (server, _) =
        insert_user_and_approve_pending_cli_device_authorization_with_browser_session(&test_db)
            .await;

    let (first, second) = tokio::join!(poll(&server), poll(&server));

    let successes = [&first, &second]
        .iter()
        .filter(|res| res.status_code().is_success())
        .count();
    assert_eq!(successes, 1);
    let sessions = test_db.rows::<session::Entity>().await;
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions
            .iter()
            .filter(|s| s.is_cli_device_sign_in_session)
            .count(),
        1
    );
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deactivated_user_device_code_poll_is_refused_with_cli_auth_expired_and_writes_no_session_or_login_success_audit_log()
 {
    let test_db = TestDatabase::create().await;
    let (server, browser_session) =
        insert_user_and_approve_pending_cli_device_authorization_with_browser_session(&test_db)
            .await;
    let mut inactive_test_user_active_model: gradient_entity::user::ActiveModel = user().into();
    inactive_test_user_active_model.active = Set(false);
    gradient_db::rows::update::<gradient_entity::user::Entity, _>(
        &test_db.connection(),
        inactive_test_user_active_model,
    )
    .await
    .unwrap();

    let res = poll(&server).await;

    res.assert_status_bad_request();
    assert_eq!(res.json::<Value>()["code"], "cli_auth_expired");
    let sessions = test_db.rows::<session::Entity>().await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, browser_session.id);
    assert!(
        !test_db
            .rows::<audit_log::Entity>()
            .await
            .iter()
            .any(|row| row.event == Action::LoginSuccess.name())
    );
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_poll_session_stores_poll_request_ip_and_user_agent() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let browser_session_row_that_approves_cli_sign_in = session::Model {
        ip: Some("198.51.100.9".into()),
        user_agent: Some("browser".into()),
        ..live_session(SessionId::now_v7())
    };
    gradient_db::rows::insert::<session::Entity, _>(
        &db,
        browser_session_row_that_approves_cli_sign_in.clone(),
    )
    .await
    .unwrap();
    gradient_db::rows::insert::<cli_device_authorization::Entity, _>(
        &db,
        pending_row("dev-code-xyz", "ABCD-EFGH"),
    )
    .await
    .unwrap();
    let server = server_behind_trusted_proxy(&test_db);
    server
        .post("/api/v1/auth/cli/authorize")
        .add_header(
            "authorization",
            bearer(browser_session_row_that_approves_cli_sign_in.id),
        )
        .add_header("x-forwarded-for", "198.51.100.9")
        .add_header("user-agent", "browser")
        .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
        .await
        .assert_status_ok();

    server
        .post("/api/v1/auth/cli/poll")
        .add_header("x-forwarded-for", "203.0.113.7")
        .add_header("user-agent", "gradient-cli")
        .json(&serde_json::json!({ "device_code": "dev-code-xyz" }))
        .await
        .assert_status_ok();

    let sessions = test_db.rows::<session::Entity>().await;
    let command_line_poll_created_session_row = sessions
        .iter()
        .find(|s| s.id != browser_session_row_that_approves_cli_sign_in.id)
        .unwrap();
    assert_eq!(
        command_line_poll_created_session_row.ip.as_deref(),
        Some("203.0.113.7")
    );
    assert_eq!(
        command_line_poll_created_session_row.user_agent.as_deref(),
        Some("gradient-cli")
    );
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn polling_pending_and_denied_device_authorizations_answers_cli_auth_pending_and_cli_auth_denied_and_keeps_rows()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let pending = pending_row("dev-code-xyz", "ABCD-EFGH");
    gradient_db::rows::insert::<cli_device_authorization::Entity, _>(&db, pending.clone())
        .await
        .unwrap();
    let server = TestState::new().web_db(db.clone()).server();

    let res = poll(&server).await;
    res.assert_status_bad_request();
    assert_eq!(res.json::<Value>()["code"], "cli_auth_pending");
    assert_eq!(
        test_db
            .rows::<cli_device_authorization::Entity>()
            .await
            .len(),
        1
    );

    let mut device_authorization_marked_denied: cli_device_authorization::ActiveModel =
        pending.into();
    device_authorization_marked_denied.denied_at = Set(Some(Utc::now().naive_utc()));
    gradient_db::rows::update::<cli_device_authorization::Entity, _>(
        &db,
        device_authorization_marked_denied,
    )
    .await
    .unwrap();

    let res = poll(&server).await;
    res.assert_status_bad_request();
    assert_eq!(res.json::<Value>()["code"], "cli_auth_denied");
    assert_eq!(
        test_db
            .rows::<cli_device_authorization::Entity>()
            .await
            .len(),
        1
    );
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_unknown_device_code_returns_404() {
    let s =
        server_with(|db| db.append_query_results([Vec::<cli_device_authorization::Model>::new()]));

    let res = s
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": "nope" }))
        .await;
    res.assert_status_not_found();
}

#[tokio::test(flavor = "multi_thread")]
async fn authorize_requires_auth() {
    let s = server_with(|db| db);

    let res = s
        .post("/api/v1/auth/cli/authorize")
        .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
        .await;
    res.assert_status_forbidden();
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_marks_row_denied() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id);
    let row = pending_row("dev-code-xyz", "ABCD-EFGH");

    let s = server_with(|db| {
        let db = auth_queue(db, session);
        db.append_query_results([vec![row.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![cli_device_authorization::Model {
                denied_at: Some(Utc::now().naive_utc()),
                ..row
            }]])
            .append_query_results([Vec::<gradient_entity::audit_log::Model>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
    });

    let res = s
        .post("/api/v1/auth/cli/deny")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({ "user_code": "ABCD-EFGH" }))
        .await;
    res.assert_status_ok();
}

const EXAMPLE_OPENSSH_ED25519_PUBLIC_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";

async fn run_cli_device_start_browser_approval_and_poll_to_get_bearer_token(
    test_db: &TestDatabase,
) -> (TestServer, session::Model, String) {
    let db = test_db.connection();
    gradient_db::rows::insert::<gradient_entity::user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let browser_session = live_session(SessionId::now_v7());
    gradient_db::rows::insert::<session::Entity, _>(&db, browser_session.clone())
        .await
        .unwrap();
    let server = TestState::new()
        .web_db(db)
        .cli(|cli| cli.ssh.enable = true)
        .server();

    let start = server.post("/api/v1/auth/cli/start").await;
    start.assert_status_ok();
    let start = start.json::<Value>();
    server
        .post("/api/v1/auth/cli/authorize")
        .add_header("authorization", bearer(browser_session.id))
        .json(&serde_json::json!({ "user_code": start["message"]["user_code"] }))
        .await
        .assert_status_ok();
    let poll = server
        .post("/api/v1/auth/cli/poll")
        .json(&serde_json::json!({ "device_code": start["message"]["device_code"] }))
        .await;
    poll.assert_status_ok();
    let cli_token = format!(
        "Bearer {}",
        poll.json::<Value>()["message"].as_str().unwrap()
    );
    (server, browser_session, cli_token)
}

#[tokio::test(flavor = "multi_thread")]
async fn session_from_cli_device_approval_cannot_create_api_keys_add_ssh_keys_change_settings_or_approve_device_codes()
 {
    let test_db = TestDatabase::create().await;
    let (server, browser_session, cli_token) =
        run_cli_device_start_browser_approval_and_poll_to_get_bearer_token(&test_db).await;
    let pending = server.post("/api/v1/auth/cli/start").await.json::<Value>();
    let user_code = pending["message"]["user_code"].as_str().unwrap().to_owned();
    let code_body = serde_json::json!({ "user_code": user_code });
    let unknown_id = Uuid::now_v7();
    let snapshot = || async {
        (
            test_db.rows::<gradient_entity::api::Entity>().await,
            test_db
                .rows::<gradient_entity::user_ssh_key::Entity>()
                .await,
            test_db.rows::<gradient_entity::user::Entity>().await,
            test_db.rows::<cli_device_authorization::Entity>().await,
            test_db
                .rows::<session::Entity>()
                .await
                .into_iter()
                .map(|row| (row.id, row.revoked_at))
                .collect::<Vec<_>>(),
        )
    };
    let before = snapshot().await;

    let refused = [
        (
            "create API key",
            Method::POST,
            "/api/v1/user/keys".to_owned(),
            serde_json::json!({ "name": "from-cli", "permissions": ["viewProject"] }),
        ),
        (
            "change API key",
            Method::PATCH,
            format!("/api/v1/user/keys/{unknown_id}"),
            serde_json::json!({ "name": "renamed" }),
        ),
        (
            "delete API key",
            Method::DELETE,
            "/api/v1/user/keys".to_owned(),
            serde_json::json!({ "name": "from-cli" }),
        ),
        (
            "revoke API key",
            Method::POST,
            format!("/api/v1/user/keys/{unknown_id}/revoke"),
            Value::Null,
        ),
        (
            "add SSH key",
            Method::POST,
            "/api/v1/user/ssh-keys".to_owned(),
            serde_json::json!({ "name": "laptop", "public_key": EXAMPLE_OPENSSH_ED25519_PUBLIC_KEY }),
        ),
        (
            "delete SSH key",
            Method::DELETE,
            format!("/api/v1/user/ssh-keys/{unknown_id}"),
            Value::Null,
        ),
        (
            "change settings",
            Method::PATCH,
            "/api/v1/user/settings".to_owned(),
            serde_json::json!({ "email": "attacker@example.com" }),
        ),
        (
            "delete account",
            Method::DELETE,
            "/api/v1/user".to_owned(),
            Value::Null,
        ),
        (
            "link sign-in provider",
            Method::GET,
            "/api/v1/user/identities/github/connect".to_owned(),
            Value::Null,
        ),
        (
            "unlink sign-in provider",
            Method::DELETE,
            "/api/v1/user/identities/github".to_owned(),
            Value::Null,
        ),
        (
            "read device code",
            Method::GET,
            format!("/api/v1/auth/cli/info?user_code={user_code}"),
            Value::Null,
        ),
        (
            "approve device code",
            Method::POST,
            "/api/v1/auth/cli/authorize".to_owned(),
            code_body.clone(),
        ),
        (
            "deny device code",
            Method::POST,
            "/api/v1/auth/cli/deny".to_owned(),
            code_body.clone(),
        ),
        (
            "end other session",
            Method::DELETE,
            format!("/api/v1/user/sessions/{}", browser_session.id),
            Value::Null,
        ),
    ];
    for (route_class, method, path, body) in refused {
        let res = server
            .method(method, &path)
            .add_header("authorization", cli_token.as_str())
            .json(&body)
            .await;
        assert_eq!(res.status_code(), 403, "{route_class}: {}", res.text());
        assert_eq!(
            res.json::<Value>()["code"],
            "cli_auth_needs_browser_session",
            "{route_class}"
        );
    }
    assert!(snapshot().await == before, "a refused request changed rows");

    let browser_token = bearer(browser_session.id);
    for (method, path, body) in [
        (
            Method::POST,
            "/api/v1/user/keys".to_owned(),
            serde_json::json!({ "name": "from-browser", "permissions": ["viewProject"] }),
        ),
        (
            Method::POST,
            "/api/v1/user/ssh-keys".to_owned(),
            serde_json::json!({ "name": "laptop", "public_key": EXAMPLE_OPENSSH_ED25519_PUBLIC_KEY }),
        ),
        (
            Method::PATCH,
            "/api/v1/user/settings".to_owned(),
            serde_json::json!({ "name": "Renamed User" }),
        ),
        (
            Method::GET,
            format!("/api/v1/auth/cli/info?user_code={user_code}"),
            Value::Null,
        ),
        (
            Method::POST,
            "/api/v1/auth/cli/authorize".to_owned(),
            code_body,
        ),
    ] {
        let res = server
            .method(method, &path)
            .add_header("authorization", browser_token.as_str())
            .json(&body)
            .await;
        assert_eq!(res.status_code(), 200, "{path}: {}", res.text());
    }
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_list_marks_cli_session_but_not_browser_session_after_browser_approved_cli_sign_in()
{
    let test_db = TestDatabase::create().await;
    let (server, browser_session, cli_token) =
        run_cli_device_start_browser_approval_and_poll_to_get_bearer_token(&test_db).await;

    let res = server
        .get("/api/v1/user/sessions")
        .add_header("authorization", cli_token.as_str())
        .await;
    res.assert_status_ok();
    let returned_session_items = res.json::<Value>()["message"]["items"].clone();
    let returned_session_items = returned_session_items.as_array().unwrap();
    assert_eq!(returned_session_items.len(), 2);
    for session in returned_session_items {
        let session_item_is_cli_approving_browser_session =
            session["id"] == browser_session.id.to_string();
        assert_eq!(
            session["is_cli_device_sign_in_session"],
            Value::Bool(!session_item_is_cli_approving_browser_session),
            "{session}"
        );
    }
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_from_cli_device_approval_can_read_own_user_list_projects_trigger_evaluation_and_revoke_own_session()
 {
    let test_db = TestDatabase::create().await;
    let (server, _, cli_token) =
        run_cli_device_start_browser_approval_and_poll_to_get_bearer_token(&test_db).await;
    let cli_session_id = test_db
        .rows::<session::Entity>()
        .await
        .into_iter()
        .find(|row| row.is_cli_device_sign_in_session)
        .unwrap()
        .id;

    for path in [
        "/api/v1/user",
        "/api/v1/user/keys",
        "/api/v1/user/sessions",
        "/api/v1/projects",
    ] {
        let res = server
            .get(path)
            .add_header("authorization", cli_token.as_str())
            .await;
        assert_eq!(res.status_code(), 200, "{path}: {}", res.text());
    }
    server
        .post("/api/v1/tasks/unknown-project/unknown-task/evaluate")
        .add_header("authorization", cli_token.as_str())
        .await
        .assert_status_not_found();
    server
        .delete(&format!("/api/v1/user/sessions/{cli_session_id}"))
        .add_header("authorization", cli_token.as_str())
        .await
        .assert_status_ok();
    server
        .get("/api/v1/user")
        .add_header("authorization", cli_token.as_str())
        .await
        .assert_status_unauthorized();
    test_db.finish().await;
}
