/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! PKCE regression tests (issue #318): the authorization redirect must carry
//! `code_challenge` + `code_challenge_method=S256`, and the verifier stored in
//! the signed `oidc_csrf` cookie must hash to that challenge.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::cli::OidcArgs;
use gradient_core::types::{RuntimeConfig, ServerState, WebDb, WorkerDb};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use sea_orm::{DatabaseBackend, MockDatabase};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use test_support::cli::test_cli;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::log_storage::NoopLogStorage;
use url::Url;
use uuid::Uuid;
use web::create_router;

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

    let mut cli = test_cli();
    cli.secrets.jwt_secret_file = jwt_path.to_string_lossy().into_owned();
    cli.oidc = OidcArgs {
        oidc_enabled: true,
        oidc_required: false,
        oidc_client_id: Some("test-client".into()),
        oidc_client_secret_file: Some(secret_path.to_string_lossy().into_owned()),
        oidc_scopes: None,
        oidc_discovery_url: Some(base.clone()),
    };

    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: Arc::new(std::collections::HashMap::new()),
        board_events: tokio::sync::broadcast::channel(256).0,
    });

    let server = axum_test::TestServer::new(create_router(state));
    let res = server.get("/api/v1/auth/oidc/login").await;
    res.assert_status(axum::http::StatusCode::FOUND);

    let location = res.header("location");
    let auth_url = Url::parse(location.to_str().unwrap()).unwrap();
    let params: std::collections::HashMap<_, _> =
        auth_url.query_pairs().into_owned().collect();

    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let challenge = params.get("code_challenge").expect("code_challenge present");

    let cookie = res.header("set-cookie");
    let csrf = jwt_decode(cookie.to_str().unwrap());

    let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(csrf.pkce_verifier.as_bytes()));
    assert_eq!(&expected, challenge, "challenge must be S256(verifier)");
    assert!(!csrf.state.is_empty() && !csrf.nonce.is_empty());
}
