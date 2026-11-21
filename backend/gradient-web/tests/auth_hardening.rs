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
use axum_test::{TestResponse, TestServer};
use chrono::{Duration, Utc};
use gradient_entity::{api, session};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, user};
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_types::{ApiId, ConcurrencyPolicy, SessionId, UserId};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{IdenStatic, Iterable, MockDatabase, MockExecResult, ModelTrait};
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

fn sign_session_jwt(user_id: UserId, session_id: SessionId, lifetime: Duration) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp(),
        exp: (now + lifetime).timestamp(),
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

fn hash_api_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

fn server_with(web_db_setup: impl FnOnce(MockDatabase) -> MockDatabase) -> TestServer {
    TestState::new()
        .web_db(web_db_setup(mock_db()).into_connection())
        .server()
}

fn revoked_session() -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id: SessionId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap()),
        user_id: user_id(),
        created_at: now,
        expires_at: now + chrono::Duration::hours(1),
        last_used_at: now,
        revoked_at: Some(now),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_with_revoked_session_is_rejected() {
    let session = revoked_session();
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| db.append_query_results([vec![session]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_with_unknown_session_is_rejected() {
    let token = sign_session_jwt(user_id(), SessionId::now_v7(), Duration::hours(1));

    let s = server_with(|db| db.append_query_results([Vec::<session::Model>::new()]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn jwt_with_expired_session_is_rejected() {
    let now = Utc::now().naive_utc();
    let mut session = live_session(SessionId::now_v7());
    session.expires_at = now - chrono::Duration::seconds(1);
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| db.append_query_results([vec![session]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_api_key_is_rejected() {
    let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "leaked".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        revoked_at: Some(now),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| db.append_query_results([vec![key]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_api_key_is_rejected() {
    let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "expired".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        expires_at: Some(now - chrono::Duration::seconds(1)),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| db.append_query_results([vec![key]]));

    let res = s
        .get("/api/v1/user")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status_unauthorized();
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

#[tokio::test(flavor = "multi_thread")]
async fn delete_user_without_password_is_forbidden() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| auth_queue(db, session));

    let res = s
        .delete("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({}))
        .await;
    res.assert_status_forbidden();
    let body: Value = res.json();
    assert_eq!(body["error"], Value::Bool(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn login_to_an_unknown_or_passwordless_account_fails_like_a_wrong_password() {
    let passwordless = gradient_entity::user::Model {
        password: None,
        ..user()
    };
    for accounts in [vec![], vec![passwordless]] {
        let s = server_with(|db| db.append_query_results([accounts]));

        let res = s
            .post("/api/v1/auth/basic/login")
            .json(&serde_json::json!({ "loginname": "someone", "password": "guess" }))
            .await;

        res.assert_status_unauthorized();
        let body: Value = res.json();
        assert_eq!(body["code"], "invalid_credentials");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_with_zero_day_expiry_is_rejected() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| auth_queue(db, session));

    let res = s
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "name": "short-lived",
            "permissions": ["view"],
            "expires_in_days": 0,
        }))
        .await;
    res.assert_status_bad_request();
    let body: Value = res.json();
    assert_eq!(body["message"], "Set API key expiry to at least 1 day.");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_user_with_wrong_password_is_forbidden() {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

    let s = server_with(|db| auth_queue(db, session));

    let res = s
        .delete("/api/v1/user")
        .add_header("authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({ "password": "WrongPassword!" }))
        .await;
    res.assert_status_forbidden();
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_with_only_view_cannot_trigger_evaluation() {
    use gradient_fixtures::ids::project_id;
    use gradient_fixtures::rows::project;
    use gradient_types::permissions::{Permission, mask_from};
    let raw = "x".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "ci".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(&[Permission::ViewProject]),
        ..Default::default()
    };
    let admin_membership = gradient_entity::project_user::Model {
        id: gradient_entity::ids::ProjectUserId::now_v7(),
        project: project_id(),
        user: user_id(),
        role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
    };
    let admin_role = gradient_entity::role::Model {
        id: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
            .append_query_results([vec![project()]])
            .append_query_results([vec![gradient_entity::task::Model {
                id: gradient_fixtures::ids::task_id(),
                project: project_id(),
                name: "test-task".into(),
                display_name: "Test".into(),
                repository: "git@example.com:test/test.git".into(),
                wildcard: "*".into(),
                active: true,
                last_check_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
                created_by: user_id(),
                created_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
                keep_evaluations: 30,
                concurrency: ConcurrencyPolicy::Skip,
                ..Default::default()
            }]])
            .append_query_results([vec![admin_membership]])
            .append_query_results([vec![admin_role]])
    });

    let res = s
        .post("/api/v1/tasks/test-project/test-task/evaluate")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_pinned_to_other_project_is_invisible() {
    use gradient_fixtures::rows::project;
    use gradient_types::permissions::{Permission, mask_from};
    let raw = "y".repeat(64);
    let now = Utc::now().naive_utc();
    let pinned_elsewhere =
        gradient_entity::ids::ProjectId::new(uuid::uuid!("ffffffff-ffff-ffff-ffff-ffffffffffff"));
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "ci".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(Permission::ALL),
        project: Some(pinned_elsewhere),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
            .append_query_results([vec![project()]])
    });

    let res = s
        .get("/api/v1/projects/test-project")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .await;
    res.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_cannot_create_api_keys() {
    let raw = "z".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "self".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
    });

    let res = s
        .post("/api/v1/user/keys")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .json(&serde_json::json!({
            "name": "child",
            "permissions": ["viewProject"],
        }))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_cannot_add_ssh_keys() {
    let raw = "y".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "self".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: gradient_types::permissions::admin_mask(),
        ..Default::default()
    };

    let s = server_with(|db| {
        db.append_query_results([vec![key.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![key.clone()]])
            .append_query_results([vec![user()]])
    });

    let res = s
        .post("/api/v1/user/ssh-keys")
        .add_header("authorization", format!("Bearer GRAD{}", raw))
        .json(&serde_json::json!({
            "name": "laptop",
            "public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test",
        }))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn project_pinned_view_key_cannot_reach_account_routes() {
    use axum::http::Method;
    use gradient_types::permissions::{Permission, mask_from};
    let raw = "p".repeat(64);
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "sandbox-view".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        permission: mask_from(&[Permission::ViewProject]),
        project: Some(gradient_entity::ids::ProjectId::now_v7()),
        ..Default::default()
    };
    let routes = [
        (Method::GET, "/api/v1/user/keys"),
        (Method::GET, "/api/v1/user/sessions"),
        (Method::GET, "/api/v1/user/audit-log"),
        (Method::GET, "/api/v1/user/settings"),
        (Method::PATCH, "/api/v1/user/settings"),
        (Method::GET, "/api/v1/user/requests"),
        (Method::DELETE, "/api/v1/user"),
        (Method::POST, "/api/v1/auth/cli/authorize"),
    ];

    for (method, path) in routes {
        let s = server_with(|db| {
            db.append_query_results([vec![key.clone()]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .append_query_results([vec![key.clone()]])
                .append_query_results([vec![user()]])
        });
        let res = s
            .method(method.clone(), path)
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .json(&serde_json::json!({}))
            .await;
        assert_eq!(
            res.status_code(),
            axum::http::StatusCode::FORBIDDEN,
            "{method} {path}"
        );
    }
}

const FIXTURE_USER_PASSWORD: &str = "TestPass123!";

fn build_test_server_with_trusted_proxy_peer(db: MockDatabase) -> TestServer {
    let router = gradient_web::create_router(TestState::new().web_db(db.into_connection()).build())
        .expect("router")
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    TestServer::new(router)
}

async fn send_password_sign_in_request_from_client_ip(
    server: &TestServer,
    client_ip: &str,
    loginname: &str,
    password: &str,
) -> TestResponse {
    server
        .post("/api/v1/auth/basic/login")
        .add_header("x-forwarded-for", client_ip)
        .json(&serde_json::json!({ "loginname": loginname, "password": password }))
        .await
}

fn queue_successful_password_sign_in_query_results(
    db: MockDatabase,
    account: &gradient_entity::user::Model,
) -> MockDatabase {
    add_audit_log_insert_result_to_mock_database(
        db.append_query_results([vec![account.clone()]])
            .append_query_results([vec![live_session(SessionId::now_v7())]])
            .append_query_results([vec![account.clone()]]),
    )
}

fn add_audit_log_insert_result_to_mock_database(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<gradient_entity::audit_log::Model>::new()])
}

fn add_rejected_password_sign_in_query_results_to_mock_database(
    mut db: MockDatabase,
    account_rows: &[gradient_entity::user::Model],
    attempts: usize,
) -> MockDatabase {
    for _ in 0..attempts {
        db = add_audit_log_insert_result_to_mock_database(
            db.append_query_results([account_rows.to_vec()]),
        );
    }
    db
}

fn assert_sign_in_response_is_rate_limited_wait(response: &TestResponse) {
    response.assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.header("retry-after"), "1");
    assert_eq!(response.json::<Value>()["code"], "rate_limit_exceeded");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_passwords_from_many_networks_delay_the_account_without_running_password_check() {
    let account = user();
    let account_rows = [account.clone()];
    let mut db =
        add_rejected_password_sign_in_query_results_to_mock_database(mock_db(), &account_rows, 5);
    for _ in 0..2 {
        db = db
            .append_query_results([account_rows.to_vec()])
            .append_query_results([Vec::<session::Model>::new()]);
    }
    db = db.append_query_results([account_rows.to_vec()]);
    db = queue_successful_password_sign_in_query_results(db, &account);
    db = add_rejected_password_sign_in_query_results_to_mock_database(db, &account_rows, 2);
    let server = build_test_server_with_trusted_proxy_peer(db);

    for i in 1..=5 {
        send_password_sign_in_request_from_client_ip(
            &server,
            &format!("198.51.100.{i}"),
            &account.username,
            "wrong",
        )
        .await
        .assert_status_unauthorized();
    }
    assert_sign_in_response_is_rate_limited_wait(
        &send_password_sign_in_request_from_client_ip(
            &server,
            "198.51.100.6",
            &account.username,
            "wrong",
        )
        .await,
    );
    assert_sign_in_response_is_rate_limited_wait(
        &send_password_sign_in_request_from_client_ip(
            &server,
            "198.51.100.7",
            &account.username,
            FIXTURE_USER_PASSWORD,
        )
        .await,
    );

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    send_password_sign_in_request_from_client_ip(
        &server,
        "198.51.100.8",
        &account.username,
        FIXTURE_USER_PASSWORD,
    )
    .await
    .assert_status_ok();

    for i in 9..=10 {
        send_password_sign_in_request_from_client_ip(
            &server,
            &format!("198.51.100.{i}"),
            &account.username,
            "wrong",
        )
        .await
        .assert_status_unauthorized();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_wrong_password_sign_in_from_twelve_client_ips_allows_at_most_five_401_responses_and_429_for_the_rest()
 {
    let account = user();
    let mock_row_with_user_and_session_columns_and_no_ip =
        build_mock_row_with_user_and_session_columns_and_no_ip(&account);
    let server = build_test_server_with_trusted_proxy_peer(mock_db().append_query_results(
        (0..36).map(|_| vec![mock_row_with_user_and_session_columns_and_no_ip.clone()]),
    ));

    let twelve_parallel_wrong_password_sign_in_responses =
        futures::future::join_all((1..=12).map(|i| {
            let server = &server;
            let username = &account.username;
            async move {
                send_password_sign_in_request_from_client_ip(
                    server,
                    &format!("198.51.100.{i}"),
                    username,
                    "wrong",
                )
                .await
            }
        }))
        .await;

    let invalid_credentials_response_count = twelve_parallel_wrong_password_sign_in_responses
        .iter()
        .filter(|response| response.status_code() == axum::http::StatusCode::UNAUTHORIZED)
        .count();
    let too_many_requests_response_count = twelve_parallel_wrong_password_sign_in_responses
        .iter()
        .filter(|response| response.status_code() == axum::http::StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(
        invalid_credentials_response_count, 5,
        "{invalid_credentials_response_count} returned HTTP 401"
    );
    assert_eq!(too_many_requests_response_count, 7);
}

fn build_mock_row_with_user_and_session_columns_and_no_ip(
    account: &gradient_entity::user::Model,
) -> std::collections::BTreeMap<String, sea_orm::Value> {
    let live_signed_in_session_row_without_ip_address = live_session(SessionId::now_v7());
    session::Column::iter()
        .map(|column| {
            (
                column.as_str().to_owned(),
                live_signed_in_session_row_without_ip_address.get(column),
            )
        })
        .chain(
            gradient_entity::user::Column::iter()
                .map(|column| (column.as_str().to_owned(), account.get(column))),
        )
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn correct_password_from_network_with_live_session_is_not_delayed() {
    let account = user();
    let session_of_signed_in_owner = session::Model {
        ip: Some("2001:db8:7:7::1".to_owned()),
        ..live_session(SessionId::now_v7())
    };
    let db = add_rejected_password_sign_in_query_results_to_mock_database(
        mock_db(),
        std::slice::from_ref(&account),
        5,
    );
    let db = db
        .append_query_results([vec![account.clone()]])
        .append_query_results([vec![session_of_signed_in_owner]]);
    let db = queue_successful_password_sign_in_query_results(db, &account);
    let server = build_test_server_with_trusted_proxy_peer(db);

    for i in 1..=5 {
        send_password_sign_in_request_from_client_ip(
            &server,
            &format!("198.51.100.{i}"),
            &account.username,
            "wrong",
        )
        .await
        .assert_status_unauthorized();
    }
    send_password_sign_in_request_from_client_ip(
        &server,
        "2001:db8:7:7::2",
        &account.username,
        FIXTURE_USER_PASSWORD,
    )
    .await
    .assert_status_ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn correct_password_from_network_with_live_cli_device_sign_in_session_is_still_delayed() {
    let account = user();
    let owner_cli_sign_in_device_flow_session_row = session::Model {
        ip: Some("2001:db8:7:7::1".to_owned()),
        is_cli_device_sign_in_session: true,
        ..live_session(SessionId::now_v7())
    };
    let db = add_rejected_password_sign_in_query_results_to_mock_database(
        mock_db(),
        std::slice::from_ref(&account),
        5,
    );
    let db = db
        .append_query_results([vec![account.clone()]])
        .append_query_results([vec![owner_cli_sign_in_device_flow_session_row]]);
    let db = queue_successful_password_sign_in_query_results(db, &account);
    let server = build_test_server_with_trusted_proxy_peer(db);

    for i in 1..=5 {
        send_password_sign_in_request_from_client_ip(
            &server,
            &format!("198.51.100.{i}"),
            &account.username,
            "wrong",
        )
        .await
        .assert_status_unauthorized();
    }
    send_password_sign_in_request_from_client_ip(
        &server,
        "2001:db8:7:7::2",
        &account.username,
        FIXTURE_USER_PASSWORD,
    )
    .await
    .assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_wrong_sign_in_attempts_for_email_without_account_return_invalid_credentials_not_too_many_requests()
 {
    let server = build_test_server_with_trusted_proxy_peer(
        add_rejected_password_sign_in_query_results_to_mock_database(mock_db(), &[], 8),
    );

    for i in 1..=8 {
        let response = send_password_sign_in_request_from_client_ip(
            &server,
            &format!("198.51.100.{i}"),
            "nobody@example.com",
            "wrong",
        )
        .await;
        response.assert_status_unauthorized();
        assert_eq!(response.json::<Value>()["code"], "invalid_credentials");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_wrong_sign_in_attempts_for_account_that_signs_in_only_through_external_identity_provider_return_invalid_credentials_not_too_many_requests()
 {
    let account_without_password = gradient_entity::user::Model {
        password: None,
        ..user()
    };
    let server = build_test_server_with_trusted_proxy_peer(
        add_rejected_password_sign_in_query_results_to_mock_database(
            mock_db(),
            std::slice::from_ref(&account_without_password),
            8,
        ),
    );

    for i in 1..=8 {
        let response = send_password_sign_in_request_from_client_ip(
            &server,
            &format!("198.51.100.{i}"),
            &account_without_password.username,
            "wrong",
        )
        .await;
        response.assert_status_unauthorized();
        assert_eq!(response.json::<Value>()["code"], "invalid_credentials");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn five_wrong_password_account_deletion_confirmations_delay_next_password_sign_in_from_network_without_live_session_with_429_and_retry_after()
 {
    let session = live_session(SessionId::now_v7());
    let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));
    let mut db = auth_queue(mock_db(), session.clone());
    for _ in 1..5 {
        db = db
            .append_query_results([vec![session.clone()]])
            .append_query_results([vec![user()]]);
    }
    db = db
        .append_query_results([vec![user()]])
        .append_query_results([Vec::<session::Model>::new()]);
    let server = build_test_server_with_trusted_proxy_peer(db);

    for _ in 0..5 {
        server
            .delete("/api/v1/user")
            .add_header("authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({ "password": "WrongPassword!" }))
            .await
            .assert_status_forbidden();
    }
    assert_sign_in_response_is_rate_limited_wait(
        &send_password_sign_in_request_from_client_ip(
            &server,
            "198.51.100.20",
            &user().username,
            FIXTURE_USER_PASSWORD,
        )
        .await,
    );
}
