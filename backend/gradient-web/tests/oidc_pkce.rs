/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]
#![allow(clippy::disallowed_methods, reason = "test harness server")]

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use gradient_test_support::state::TestState;
use gradient_types::cli::OidcArgs;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

#[derive(Deserialize)]
struct CsrfClaims {
    state: String,
    nonce: String,
    pkce_verifier: String,
}

async fn metadata(State(base): State<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks"),
    }))
}

async fn spawn_idp() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(metadata))
        .with_state(base.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn jwt_decode(cookie: &str) -> CsrfClaims {
    let jwt = cookie
        .split("oidc_csrf=")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("oidc_csrf cookie present");
    decode::<CsrfClaims>(
        jwt,
        &DecodingKey::from_secret(b"test-jwt-secret"),
        &Validation::new(Algorithm::HS256),
    )
    .expect("decode CSRF cookie")
    .claims
}

#[tokio::test]
async fn authorize_redirect_carries_pkce_and_cookie_holds_verifier() {
    let base = spawn_idp().await;

    let tmp = std::env::temp_dir();
    let suffix = Uuid::now_v7();
    let jwt_path = tmp.join(format!("gradient-pkce-jwt-{suffix}"));
    std::fs::write(&jwt_path, "test-jwt-secret").unwrap();
    let secret_path = tmp.join(format!("gradient-pkce-secret-{suffix}"));
    std::fs::write(&secret_path, "test-client-secret").unwrap();

    let server = TestState::new()
        .cli(|cli| {
            cli.secrets.jwt_file = jwt_path.to_string_lossy().into_owned();
            cli.oidc = OidcArgs {
                enable: true,
                required: false,
                client_id: Some("test-client".into()),
                client_secret_file: Some(secret_path.to_string_lossy().into_owned()),
                discovery_url: Some(base.clone()),
                ..Default::default()
            };
        })
        .server();

    let res = server.get("/api/v1/auth/oidc/login").await;
    res.assert_status(axum::http::StatusCode::FOUND);

    let location = res.header("location");
    let auth_url = Url::parse(location.to_str().unwrap()).unwrap();
    let params: std::collections::HashMap<_, _> = auth_url.query_pairs().into_owned().collect();

    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let challenge = params
        .get("code_challenge")
        .expect("code_challenge present");

    let cookie = res.header("set-cookie");
    let csrf = jwt_decode(cookie.to_str().unwrap());

    let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.pkce_verifier.as_bytes()));
    assert_eq!(&expected, challenge, "challenge must be S256(verifier)");
    assert!(!csrf.state.is_empty() && !csrf.nonce.is_empty());
}
