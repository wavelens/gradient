/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared helpers for `web` crate integration tests.
//!
//! Centralises the boilerplate that every `tests/*.rs` file in `web` needs:
//! issuing a session JWT, building a `session::Model` row to satisfy the auth
//! middleware, and assembling a `ServerState` + `axum_test::TestServer` from a
//! `MockDatabase` connection.
//!
//! The shared JWT secret is [`TEST_JWT_SECRET`]; it must match the one baked
//! into [`crate::state::test_state`] so handler-level state factories stay
//! interchangeable with this module's [`make_test_server`].

use std::sync::Arc;

use axum_test::TestServer;
use chrono::{Duration, Utc};
use entity::ids::{SessionId, UserId};
use entity::session;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, WebDb, WorkerDb};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use serde::Serialize;

use crate::cli::{test_cli, test_cli_with_crypt};
use crate::fakes::email::InMemoryEmailSender;
use crate::fixtures::user_id;
use crate::log_storage::NoopLogStorage;

/// Shared HMAC secret used for signing test session tokens. Must equal the
/// `jwt_secret` that [`crate::state::test_state`] and [`make_test_server`]
/// install on `ServerState`.
pub const TEST_JWT_SECRET: &str = "test-jwt-secret";

#[derive(Serialize)]
struct Claims {
    exp: usize,
    iat: usize,
    id: UserId,
    jti: SessionId,
}

/// Sign a session JWT for the canonical test user (`fixtures::user_id`) with a
/// one-hour expiry. Pair with [`live_session`] to satisfy the auth middleware.
pub fn make_token(session_id: SessionId) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp() as usize,
        exp: (now + Duration::hours(1)).timestamp() as usize,
        id: user_id(),
        jti: session_id,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("sign jwt")
}

/// A non-revoked session row matching [`make_token`]'s claims so the auth
/// middleware accepts the issued token.
pub fn live_session(id: SessionId) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        last_used_at: now,
        ..Default::default()
    }
}

/// Build a fully-wired `axum_test::TestServer` rooted at `web::create_router`,
/// using `db` as the web pool and an empty mock for the worker pool.
///
/// `crypt_secret_file` defaults to `cli::test_cli`'s placeholder, which works
/// for handlers that never read the crypt secret. Pass `Some(path)` for
/// handlers that call into `generate_signing_key`, `decrypt_signing_key`, or
/// `encrypt_secret_with_file`.
pub fn make_test_server(db: DatabaseConnection) -> TestServer {
    make_test_server_with(db, None)
}

/// Variant of [`make_test_server`] that lets callers point the crypt secret
/// file at a real on-disk path (typically a `tempfile::NamedTempFile`).
pub fn make_test_server_with(
    db: DatabaseConnection,
    crypt_secret_file: Option<String>,
) -> TestServer {
    let cli = match crypt_secret_file {
        Some(path) => test_cli_with_crypt(path),
        None => test_cli(),
    };
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(TEST_JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
    });
    TestServer::new(web::create_router(state))
}
