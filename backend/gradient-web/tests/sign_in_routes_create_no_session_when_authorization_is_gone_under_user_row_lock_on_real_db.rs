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

//! All of the tests delete the fact that allowed sign-in after the route checked that fact and before the route created a session. Routes that create a session without the lock on the user row and a recheck fail these tests.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_test::TestResponse;
use gradient_core::{AuthorizeRequest, CodeExchange, ExternalIdentity, LoginProvider};
use gradient_entity::{cli_device_authorization, session, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_test_support::state::{TEST_JWT_SECRET, TestState};
use gradient_test_support::web::bearer;
use gradient_types::cli::OidcArgs;
use gradient_types::ids::{CliDeviceAuthorizationId, SessionId};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use sea_orm::{ActiveValue::Set, EntityTrait, TransactionTrait};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

const PROVIDER_ISSUER: &str = "https://provider.example.com";
const SUBJECT: &str = "42";
const OIDC_CLIENT_ID: &str = "test-client";
const OIDC_NONCE: &str = "test-nonce";
const ED25519_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIPLZC3Vj7rLv/HiDPacoLLTGMvDER9WhbA5mY11Xz1yU
-----END PRIVATE KEY-----
";
const ED25519_PUBLIC_KEY_BASE64URL: &str = "b5NvI6C0jOd_5yHnQ1NFNG7nGhehEpZZ2-iJ33SWkOM";

#[derive(Debug)]
struct FixedExternalIdentityLoginProvider;

#[async_trait::async_trait]
impl LoginProvider for FixedExternalIdentityLoginProvider {
    fn id(&self) -> &str {
        "example"
    }
    fn label(&self) -> &str {
        "Example"
    }
    fn issuer(&self) -> &str {
        PROVIDER_ISSUER
    }
    fn replaces_password_login(&self) -> bool {
        false
    }
    fn authorize_url(&self, _: AuthorizeRequest<'_>) -> anyhow::Result<Url> {
        unimplemented!()
    }
    async fn identity(
        &self,
        _: &reqwest::Client,
        _: CodeExchange<'_>,
    ) -> anyhow::Result<ExternalIdentity> {
        Ok(ExternalIdentity {
            issuer: PROVIDER_ISSUER.into(),
            subject: SUBJECT.into(),
            username: "testuser".into(),
            email: Some("test@example.com".into()),
            name: None,
            groups: None,
            refresh_profile: false,
        })
    }
}

async fn poll_http_request_until_postgres_backend_waits_on_lock<F: Future>(
    test_db: &TestDatabase,
    mut request: Pin<&mut F>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if tokio::time::timeout(Duration::from_millis(50), request.as_mut())
            .await
            .is_ok()
        {
            panic!("The request completed before waiting on the lock on the user row.");
        }
        let waiting = test_db
            .texts(
                "SELECT pid::text FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .await;
        if !waiting.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "The request did not wait for the lock on the user row within 30 seconds."
        );
    }
}

async fn install_deferred_constraint_trigger_that_deletes_identity_links_after_account_update_commit(
    test_db: &TestDatabase,
) {
    test_db
        .execute(
            "CREATE FUNCTION delete_identity_links_of_updated_account() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN \
             DELETE FROM user_identity WHERE \"user\" = NEW.id; RETURN NULL; END $$",
        )
        .await;
    test_db
        .execute(
            "CREATE CONSTRAINT TRIGGER delete_identity_links_of_updated_account \
             AFTER UPDATE ON \"user\" DEFERRABLE INITIALLY DEFERRED FOR EACH ROW \
             EXECUTE FUNCTION delete_identity_links_of_updated_account()",
        )
        .await;
}

async fn insert_account_with_user_identity_and_return_account(
    test_db: &TestDatabase,
    issuer: &str,
) -> user::Model {
    let db = test_db.connection();
    let account = gradient_db::rows::insert::<user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    gradient_db::user_identities::insert(&db, account.id, issuer, SUBJECT)
        .await
        .unwrap();
    account
}

async fn oidc_discovery(State(base): State<String>) -> Json<Value> {
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks"),
        "id_token_signing_alg_values_supported": ["EdDSA"],
    }))
}

async fn oidc_token(State(base): State<String>) -> Json<Value> {
    let now = chrono::Utc::now().timestamp();
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("test-key".into());
    let id_token = encode(
        &header,
        &json!({
            "iss": base,
            "sub": SUBJECT,
            "aud": OIDC_CLIENT_ID,
            "iat": now,
            "exp": now + 600,
            "nonce": OIDC_NONCE,
            "preferred_username": "testuser",
        }),
        &EncodingKey::from_ed_pem(ED25519_PRIVATE_KEY_PEM.as_bytes()).unwrap(),
    )
    .unwrap();
    Json(json!({ "access_token": "access", "token_type": "Bearer", "id_token": id_token }))
}

async fn oidc_jwks() -> Json<Value> {
    Json(json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "x": ED25519_PUBLIC_KEY_BASE64URL,
            "kid": "test-key",
            "alg": "EdDSA",
            "use": "sig",
        }]
    }))
}

async fn start_fake_openid_connect_identity_provider_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(oidc_discovery))
        .route("/token", post(oidc_token))
        .route("/jwks", get(oidc_jwks))
        .with_state(base.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn signed_cookie(name: &str, claims: Value) -> HeaderValue {
    let jwt = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()).unwrap(),
    )
    .unwrap();
    HeaderValue::from_str(&format!("{name}={jwt}")).unwrap()
}

fn sets_session_cookie(response: &TestResponse) -> bool {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .any(|cookie| cookie.to_str().unwrap().starts_with("jwt_token="))
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_with_password_returns_401_and_inserts_no_session_when_password_hash_changes_during_user_row_lock_wait()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let account = gradient_db::rows::insert::<user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let server = TestState::new().web_db(db.clone()).server();

    let password_change = db.begin().await.unwrap();
    let locked = gradient_db::users::lock_user_row_for_update(&password_change, account.id)
        .await
        .unwrap()
        .unwrap();
    let request = server
        .post("/api/v1/auth/basic/login")
        .json(&json!({ "loginname": "testuser", "password": "TestPass123!" }))
        .into_future();
    tokio::pin!(request);
    poll_http_request_until_postgres_backend_waits_on_lock(&test_db, request.as_mut()).await;

    let mut changed: user::ActiveModel = locked.into();
    #[expect(
        clippy::disallowed_methods,
        reason = "Test data require real Argon2 hashes. The password module of gradient-web is private, so tests cannot access that module."
    )]
    let changed_password_argon2_hash = password_auth::generate_hash("OtherPass456!");
    changed.password = Set(Some(changed_password_argon2_hash));
    gradient_db::rows::update::<user::Entity, _>(&password_change, changed)
        .await
        .unwrap();
    password_change.commit().await.unwrap();

    request.await.assert_status(StatusCode::UNAUTHORIZED);
    assert!(test_db.rows::<session::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oidc_callback_answers_401_and_inserts_no_session_row_when_identity_link_is_deleted_after_sign_in_commit_before_session_creation()
 {
    let base = start_fake_openid_connect_identity_provider_server().await;
    let test_db = TestDatabase::create().await;
    insert_account_with_user_identity_and_return_account(&test_db, &base).await;
    install_deferred_constraint_trigger_that_deletes_identity_links_after_account_update_commit(
        &test_db,
    )
    .await;

    let client_secret =
        std::env::temp_dir().join(format!("gradient-oidc-secret-{}", Uuid::now_v7()));
    std::fs::write(&client_secret, "test-client-secret").unwrap();
    let discovery_url = base.clone();
    let server = TestState::new()
        .web_db(test_db.connection())
        .cli(move |cli| {
            cli.oidc = OidcArgs {
                enable: true,
                required: false,
                client_id: Some(OIDC_CLIENT_ID.into()),
                client_secret_file: Some(client_secret.to_string_lossy().into_owned()),
                discovery_url: Some(discovery_url),
                ..Default::default()
            };
        })
        .server();
    let now = chrono::Utc::now().timestamp();

    let response = server
        .get("/api/v1/auth/oidc/callback?code=abc&state=test-state")
        .add_header(
            header::COOKIE,
            signed_cookie(
                "oidc_csrf",
                json!({
                    "exp": now + 600,
                    "iat": now,
                    "state": "test-state",
                    "nonce": OIDC_NONCE,
                    "pkce_verifier": "verifier",
                }),
            ),
        )
        .await;

    response.assert_status(StatusCode::UNAUTHORIZED);
    assert!(!sets_session_cookie(&response));
    assert!(test_db.rows::<session::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn external_login_callback_with_identity_link_deleted_after_sign_in_commit_before_session_creation_redirects_with_login_error_failed_without_session_cookie_or_session_row()
 {
    let test_db = TestDatabase::create().await;
    insert_account_with_user_identity_and_return_account(&test_db, PROVIDER_ISSUER).await;
    install_deferred_constraint_trigger_that_deletes_identity_links_after_account_update_commit(
        &test_db,
    )
    .await;
    let server = TestState::new()
        .web_db(test_db.connection())
        .login_provider(Arc::new(FixedExternalIdentityLoginProvider))
        .server();
    let now = chrono::Utc::now().timestamp();

    let response = server
        .get("/api/v1/auth/providers/example/callback?code=abc&state=s")
        .add_header(
            header::COOKIE,
            signed_cookie(
                "login_csrf",
                json!({
                    "exp": now + 600,
                    "iat": now,
                    "provider": "example",
                    "state": "s",
                    "pkce_verifier": "v",
                    "purpose": { "kind": "login" },
                }),
            ),
        )
        .await;

    response.assert_status(StatusCode::FOUND);
    assert_eq!(
        response.header("location").to_str().unwrap(),
        "/account/login?login_error=failed"
    );
    assert!(!sets_session_cookie(&response));
    assert!(test_db.rows::<session::Entity>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_device_authorize_returns_401_creates_no_session_and_leaves_device_authorization_unapproved_when_calling_session_is_revoked_while_waiting_for_user_row_lock()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let account = gradient_db::rows::insert::<user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let calling_session = rows::live_session(SessionId::now_v7());
    gradient_db::rows::insert::<session::Entity, _>(&db, calling_session.clone())
        .await
        .unwrap();
    let now = chrono::Utc::now().naive_utc();
    gradient_db::rows::insert::<cli_device_authorization::Entity, _>(
        &db,
        cli_device_authorization::Model {
            id: CliDeviceAuthorizationId::now_v7(),
            device_code_hash: "device-code-hash".into(),
            user_code: "ABCD-EFGH".into(),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let server = TestState::new().web_db(db.clone()).server();

    let sign_out = db.begin().await.unwrap();
    gradient_db::users::lock_user_row_for_update(&sign_out, account.id)
        .await
        .unwrap()
        .unwrap();
    let request = server
        .post("/api/v1/auth/cli/authorize")
        .add_header(
            header::AUTHORIZATION,
            HeaderValue::from_str(&bearer(calling_session.id)).unwrap(),
        )
        .json(&json!({ "user_code": "ABCD-EFGH" }))
        .into_future();
    tokio::pin!(request);
    poll_http_request_until_postgres_backend_waits_on_lock(&test_db, request.as_mut()).await;

    session::Entity::delete_by_id(calling_session.id)
        .exec(&sign_out)
        .await
        .unwrap();
    sign_out.commit().await.unwrap();

    request.await.assert_status(StatusCode::UNAUTHORIZED);
    assert!(test_db.rows::<session::Entity>().await.is_empty());
    let device_authorizations = test_db.rows::<cli_device_authorization::Entity>().await;
    assert_eq!(device_authorizations.len(), 1);
    assert!(device_authorizations[0].authorized_at.is_none());
    assert!(device_authorizations[0].user_id.is_none());
    assert!(device_authorizations[0].approving_session_id.is_none());
    test_db.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_device_poll_returns_expired_creates_no_session_and_deletes_device_authorization_when_approving_session_is_revoked_before_poll_on_real_postgres()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    gradient_db::rows::insert::<user::Entity, _>(&db, rows::user())
        .await
        .unwrap();
    let approving_session = rows::live_session(SessionId::now_v7());
    gradient_db::rows::insert::<session::Entity, _>(&db, approving_session.clone())
        .await
        .unwrap();
    let now = chrono::Utc::now().naive_utc();
    gradient_db::rows::insert::<cli_device_authorization::Entity, _>(
        &db,
        cli_device_authorization::Model {
            id: CliDeviceAuthorizationId::now_v7(),
            device_code_hash: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"device-code")),
            user_code: "ABCD-EFGH".into(),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let server = TestState::new().web_db(db.clone()).server();
    server
        .post("/api/v1/auth/cli/authorize")
        .add_header(
            header::AUTHORIZATION,
            HeaderValue::from_str(&bearer(approving_session.id)).unwrap(),
        )
        .json(&json!({ "user_code": "ABCD-EFGH" }))
        .await
        .assert_status_ok();

    let mut revoked: session::ActiveModel = approving_session.clone().into();
    revoked.revoked_at = Set(Some(chrono::Utc::now().naive_utc()));
    gradient_db::rows::update::<session::Entity, _>(&db, revoked)
        .await
        .unwrap();
    let response = server
        .post("/api/v1/auth/cli/poll")
        .json(&json!({ "device_code": "device-code" }))
        .await;

    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(response.json::<Value>()["code"], "cli_auth_expired");
    let sessions = test_db.rows::<session::Entity>().await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, approving_session.id);
    assert!(
        test_db
            .rows::<cli_device_authorization::Entity>()
            .await
            .is_empty()
    );
    test_db.finish().await;
}
