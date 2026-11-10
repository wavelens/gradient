/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use axum::http::{HeaderValue, StatusCode, header};
use axum_test::{TestResponse, TestServer};
use gradient_entity::integration;
use gradient_fixtures::ids::{project_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{admin_role, live_session, project, project_member};
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_test_support::web::{make_token, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use sea_orm::MockDatabase;
use serde_json::{Value, json};
use std::collections::HashMap;
use url::Url;

const CONNECT_URL: &str = "/api/v1/projects/test-project/integrations/oauth/connect?name=codeberg";

fn with_gitea_app(state: TestState) -> TestState {
    state.cli(|cli| {
        cli.gitea_app.instance_base_url = Some("https://codeberg.example/".into());
        cli.gitea_app.client_id = Some("gradient-client".into());
        cli.gitea_app.client_secret_file = Some("/run/secrets/gitea-app".into());
    })
}

fn with_project_manage(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

fn signed_cookie(session_id: SessionId, exp_offset: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let jwt = encode(
        &Header::default(),
        &json!({
            "exp": now + exp_offset,
            "iat": now,
            "state": "s",
            "pkce_verifier": "v",
            "user_id": user_id(),
            "session_id": session_id,
            "project_id": project_id(),
            "project_name": "test-project",
            "name": "codeberg",
            "display_name": "Codeberg",
        }),
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .unwrap();
    format!("git_host_oauth_connect={jwt}")
}

async fn callback(server: &TestServer, query: &str, cookie: Option<&str>) -> TestResponse {
    let request = server.get(&format!("/api/v1/integrations/oauth/callback?{query}"));
    match cookie {
        Some(cookie) => {
            request
                .add_header(header::COOKIE, HeaderValue::from_str(cookie).unwrap())
                .await
        }
        None => request.await,
    }
}

fn location(res: &TestResponse) -> String {
    res.header("location").to_str().unwrap().to_owned()
}

#[tokio::test]
async fn connect_redirects_to_the_git_host_with_pkce_and_a_signed_cookie() {
    let session_id = SessionId::now_v7();
    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([Vec::<integration::Model>::new()])
        .append_query_results([Vec::<integration::Model>::new()]);

    let res = with_gitea_app(TestState::new().web_db(db.into_connection()))
        .server()
        .get(CONNECT_URL)
        .add_header(
            header::AUTHORIZATION,
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status(StatusCode::FOUND);
    let url = Url::parse(&location(&res)).unwrap();
    assert_eq!(url.host_str(), Some("codeberg.example"));
    assert_eq!(url.path(), "/login/oauth/authorize");
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "gradient-client");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["redirect_uri"].ends_with("/api/v1/integrations/oauth/callback"));

    let set_cookie = res.header("set-cookie").to_str().unwrap().to_owned();
    let jwt = set_cookie
        .split(';')
        .next()
        .and_then(|pair| pair.strip_prefix("git_host_oauth_connect="))
        .expect("connect cookie set");
    let claims = decode::<Value>(
        jwt,
        &DecodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .unwrap()
    .claims;
    assert_eq!(claims["state"], query["state"].as_str());
    assert_eq!(claims["session_id"], json!(session_id));
    assert_eq!(claims["name"], "codeberg");
    assert_eq!(claims["display_name"], "codeberg");
}

#[tokio::test]
async fn connect_is_refused_without_a_configured_application() {
    let session_id = SessionId::now_v7();
    let db = with_project_manage(with_auth(mock_db(), session_id));

    let res = TestState::new()
        .web_db(db.into_connection())
        .server()
        .get(CONNECT_URL)
        .add_header(
            header::AUTHORIZATION,
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn connect_is_refused_for_a_taken_integration_name() {
    let session_id = SessionId::now_v7();
    let taken = integration::Model {
        project: project_id(),
        name: "codeberg".into(),
        ..Default::default()
    };
    let db = with_project_manage(with_auth(mock_db(), session_id))
        .append_query_results([vec![taken]]);

    let res = with_gitea_app(TestState::new().web_db(db.into_connection()))
        .server()
        .get(CONNECT_URL)
        .add_header(
            header::AUTHORIZATION,
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn callback_without_the_connect_cookie_is_expired() {
    let server = with_gitea_app(TestState::new()).server();

    let res = callback(&server, "code=abc&state=s", None).await;

    res.assert_status(StatusCode::FOUND);
    assert_eq!(location(&res), "/?oauth_error=expired");
}

#[tokio::test]
async fn callback_with_a_foreign_state_is_expired() {
    let server = with_gitea_app(TestState::new()).server();
    let cookie = signed_cookie(SessionId::now_v7(), 600);

    let res = callback(&server, "code=abc&state=other", Some(&cookie)).await;

    assert_eq!(location(&res), "/?oauth_error=expired");
}

#[tokio::test]
async fn callback_with_an_expired_cookie_is_expired() {
    let server = with_gitea_app(TestState::new()).server();
    let cookie = signed_cookie(SessionId::now_v7(), -3600);

    let res = callback(&server, "code=abc&state=s", Some(&cookie)).await;

    assert_eq!(location(&res), "/?oauth_error=expired");
}

#[tokio::test]
async fn callback_reports_a_denied_approval() {
    let server = with_gitea_app(TestState::new()).server();
    let cookie = signed_cookie(SessionId::now_v7(), 600);

    let res = callback(&server, "error=access_denied&state=s", Some(&cookie)).await;

    assert_eq!(
        location(&res),
        "/project/test-project/integrations?oauth_error=denied"
    );
}

#[tokio::test]
async fn callback_after_sign_out_is_refused() {
    let session_id = SessionId::now_v7();
    let revoked = gradient_entity::session::Model {
        revoked_at: Some(chrono::Utc::now().naive_utc()),
        ..live_session(session_id)
    };
    let server = with_gitea_app(
        TestState::new().web_db(
            mock_db()
                .append_query_results([vec![revoked]])
                .into_connection(),
        ),
    )
    .server();
    let cookie = signed_cookie(session_id, 600);

    let res = callback(&server, "code=abc&state=s", Some(&cookie)).await;

    assert_eq!(
        location(&res),
        "/project/test-project/integrations?oauth_error=signed_out"
    );
}
