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
use gradient_core::{
    AuthorizeRequest, CodeExchange, ExternalIdentity, LoginProvider, LoginRefusal, MatchRule,
};
use gradient_entity::api;
use gradient_entity::ids::{ApiId, SessionId};
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, user};
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_test_support::web::{bearer, with_auth_as};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use sea_orm::{MockDatabase, MockExecResult};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

const AUTHORIZE: &str = "https://provider.example.com/authorize";
const ISSUER: &str = "https://provider.example.com";

#[derive(Debug, Clone, Copy)]
enum Answer {
    Identity,
    NoVerifiedEmail,
}

#[derive(Debug)]
struct FakeProvider(Answer);

#[async_trait::async_trait]
impl LoginProvider for FakeProvider {
    fn id(&self) -> &str {
        "example"
    }
    fn label(&self) -> &str {
        "Example"
    }
    fn issuer(&self) -> &str {
        ISSUER
    }
    fn replaces_password_login(&self) -> bool {
        false
    }
    fn authorize_url(&self, request: AuthorizeRequest<'_>) -> anyhow::Result<Url> {
        Ok(Url::parse_with_params(
            AUTHORIZE,
            [
                ("redirect_uri", request.redirect_uri),
                ("state", request.state),
                ("code_challenge", request.pkce_challenge),
            ],
        )?)
    }
    async fn identity(
        &self,
        _: &reqwest::Client,
        _: CodeExchange<'_>,
    ) -> anyhow::Result<ExternalIdentity> {
        match self.0 {
            Answer::NoVerifiedEmail => Err(LoginRefusal::NoVerifiedEmail.into()),
            Answer::Identity => Ok(ExternalIdentity {
                issuer: ISSUER.into(),
                subject: "42".into(),
                username: "octo".into(),
                email: Some("octo@example.com".into()),
                name: None,
                groups: None,
                match_rule: MatchRule::VerifiedEmail,
                refresh_profile: false,
            }),
        }
    }
}

#[derive(Deserialize)]
struct CsrfClaims {
    provider: String,
    state: String,
    purpose: Value,
}

fn state(answer: Answer) -> TestState {
    TestState::new().login_provider(Arc::new(FakeProvider(answer)))
}

fn server() -> TestServer {
    state(Answer::Identity).server()
}

fn csrf_cookie(set_cookie: &str) -> &str {
    set_cookie
        .split(';')
        .next()
        .filter(|pair| pair.starts_with("login_csrf="))
        .expect("login_csrf cookie set")
}

fn decode_csrf(cookie: &str) -> CsrfClaims {
    decode::<CsrfClaims>(
        cookie.trim_start_matches("login_csrf="),
        &DecodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .unwrap()
    .claims
}

fn signed_cookie(purpose: Value, exp_offset: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let jwt = encode(
        &Header::default(),
        &json!({
            "exp": now + exp_offset,
            "iat": now,
            "provider": "example",
            "state": "s",
            "pkce_verifier": "v",
            "purpose": purpose,
        }),
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .unwrap();
    format!("login_csrf={jwt}")
}

async fn callback(server: &TestServer, query: &str, cookie: &str) -> TestResponse {
    server
        .get(&format!("/api/v1/auth/providers/example/callback?{query}"))
        .add_header(header::COOKIE, HeaderValue::from_str(cookie).unwrap())
        .await
}

fn location(res: &TestResponse) -> String {
    res.header("location").to_str().unwrap().to_owned()
}

fn sets_session(res: &TestResponse) -> bool {
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .any(|c| c.to_str().unwrap().starts_with("jwt_token="))
}

fn api_key_db(raw: &str) -> MockDatabase {
    let now = chrono::Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: user_id(),
        name: "read-only".into(),
        key: Sha256::digest(raw.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        last_used_at: now,
        created_at: now,
        permission: 1,
        ..Default::default()
    };
    mock_db()
        .append_query_results([vec![key.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![key]])
        .append_query_results([vec![user()]])
}

#[tokio::test]
async fn login_redirects_to_the_provider_with_state() {
    let res = server().get("/api/v1/auth/providers/example/login").await;
    res.assert_status(StatusCode::FOUND);

    let location = Url::parse(&location(&res)).unwrap();
    assert!(location.as_str().starts_with(AUTHORIZE));
    let params: HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert!(params["redirect_uri"].ends_with("/api/v1/auth/providers/example/callback"));

    let set_cookie = res.header("set-cookie");
    let set_cookie = set_cookie.to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Lax"));
    let claims = decode_csrf(csrf_cookie(set_cookie));
    assert_eq!(claims.state, params["state"]);
    assert_eq!(claims.provider, "example");
    assert_eq!(claims.purpose, json!({ "kind": "login" }));
}

#[tokio::test]
async fn unknown_provider_is_not_found() {
    let res = server().get("/api/v1/auth/providers/missing/login").await;
    res.assert_status_not_found();
}

#[tokio::test]
async fn callback_rejects_state_mismatch() {
    let server = server();
    let login = server.get("/api/v1/auth/providers/example/login").await;
    let set_cookie = login.header("set-cookie");
    let cookie = csrf_cookie(set_cookie.to_str().unwrap()).to_owned();

    let res = callback(&server, "code=abc&state=forged", &cookie).await;

    res.assert_status(StatusCode::FOUND);
    assert_eq!(location(&res), "/account/login?login_error=failed");
    assert!(!sets_session(&res));
}

#[tokio::test]
async fn provider_refusal_redirects_with_reason() {
    let server = state(Answer::NoVerifiedEmail).server();

    let res = callback(
        &server,
        "code=abc&state=s",
        &signed_cookie(json!({ "kind": "login" }), 600),
    )
    .await;

    res.assert_status(StatusCode::FOUND);
    assert_eq!(
        location(&res),
        "/account/login?login_error=no_verified_email"
    );
    assert!(!sets_session(&res));
}

#[tokio::test]
async fn connect_requires_session() {
    let res = server()
        .get("/api/v1/user/identities/example/connect")
        .await;
    res.assert_status_unauthorized();
}

#[tokio::test]
async fn connect_cookie_carries_user_and_session() {
    let session_id = SessionId::now_v7();
    let res = state(Answer::Identity)
        .web_db(with_auth_as(mock_db(), session_id, user()).into_connection())
        .server()
        .get("/api/v1/user/identities/example/connect")
        .add_header(
            header::AUTHORIZATION,
            HeaderValue::from_str(&bearer(session_id)).unwrap(),
        )
        .await;
    res.assert_status(StatusCode::FOUND);

    let set_cookie = res.header("set-cookie");
    let claims = decode_csrf(csrf_cookie(set_cookie.to_str().unwrap()));
    assert_eq!(
        claims.purpose,
        json!({ "kind": "connect", "user_id": user_id(), "session_id": session_id })
    );
}

#[tokio::test]
async fn connect_refused_for_api_keys() {
    let raw = "k".repeat(64);
    let res = state(Answer::Identity)
        .web_db(api_key_db(&raw).into_connection())
        .server()
        .get("/api/v1/user/identities/example/connect")
        .add_header(header::AUTHORIZATION, format!("Bearer GRAD{raw}"))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
    assert!(res.maybe_header("set-cookie").is_none());
}

#[tokio::test]
async fn disconnect_refused_for_api_keys() {
    let raw = "d".repeat(64);
    let res = state(Answer::Identity)
        .web_db(api_key_db(&raw).into_connection())
        .server()
        .delete("/api/v1/user/identities/example")
        .add_header(header::AUTHORIZATION, format!("Bearer GRAD{raw}"))
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn connect_after_sign_out_refused() {
    let session_id = SessionId::now_v7();
    let revoked = gradient_entity::session::Model {
        revoked_at: Some(chrono::Utc::now().naive_utc()),
        ..live_session(session_id)
    };
    let server = state(Answer::Identity)
        .web_db(
            mock_db()
                .append_query_results([vec![revoked]])
                .into_connection(),
        )
        .server();
    let purpose = json!({ "kind": "connect", "user_id": user_id(), "session_id": session_id });

    let res = callback(&server, "code=abc&state=s", &signed_cookie(purpose, 600)).await;

    res.assert_status(StatusCode::FOUND);
    assert_eq!(location(&res), "/settings/profile?connect_error=signed_out");
}

#[tokio::test]
async fn expired_connect_cookie_rejected() {
    let purpose =
        json!({ "kind": "connect", "user_id": user_id(), "session_id": SessionId::now_v7() });

    let res = callback(
        &server(),
        "code=abc&state=s",
        &signed_cookie(purpose, -3600),
    )
    .await;

    res.assert_status(StatusCode::FOUND);
    assert_eq!(location(&res), "/account/login?login_error=failed");
}

#[tokio::test]
async fn logout_clears_a_pending_connect() {
    let res = server().post("/api/v1/auth/logout").await;
    res.assert_status_ok();
    assert!(
        res.headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|c| c.to_str().unwrap().starts_with("login_csrf=;"))
    );
}
