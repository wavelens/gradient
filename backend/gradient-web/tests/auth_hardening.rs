/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the auth-hardening surface added under issue #91:
//! session-backed JWT revocation, API-key revocation/expiry, and the
//! re-auth requirement on `DELETE /user`. The tests drive the router via
//! `axum_test::TestServer` against a `MockDatabase` so they exercise the
//! same handlers the real server runs without needing Postgres.

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::{api, session};
use gradient_storage::{EmailSender, NarStore};
use gradient_types::{ApiId, ConcurrencyPolicy, RuntimeConfig, SecretString, SessionId, UserId};
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::fixtures::{user, user_id};
use gradient_test_support::log_storage::NoopLogStorage;
use uuid::Uuid;
use gradient_web::create_router;

const JWT_SECRET: &str = "test-jwt-secret";

#[derive(Serialize)]
struct Claims {
    exp: usize,
    iat: usize,
    id: UserId,
    jti: SessionId,
}

fn sign_session_jwt(user_id: UserId, session_id: SessionId, lifetime: Duration) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp() as usize,
        exp: (now + lifetime).timestamp() as usize,
        id: user_id,
        jti: session_id,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
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
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let db = web_db_setup(MockDatabase::new(DatabaseBackend::Postgres));
    let state = Arc::new(ServerState {
        web_db: WebDb::new(db.into_connection()),
        cache_db: gradient_db::CacheDb::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http client"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new(JWT_SECRET.to_string()),
        started_at: chrono::Utc::now(),
        pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        board_events: tokio::sync::broadcast::channel(256).0,
        forge: gradient_forge::ForgeRegistry::with_builtin(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        reactor: std::sync::Arc::new(gradient_db::NoReactor),
        sign_signal: std::sync::Arc::new(tokio::sync::Notify::new()),
    });
    TestServer::new(create_router(state))
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

fn live_session(id: SessionId) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + chrono::Duration::hours(1),
        last_used_at: now,
        ..Default::default()
    }
}

#[test]
fn jwt_with_revoked_session_is_rejected() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let session = revoked_session();
        let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

        let s = server_with(|db| db.append_query_results([vec![session]]));

        let res = s
            .get("/api/v1/user")
            .add_header("authorization", format!("Bearer {}", token))
            .await;
        res.assert_status_unauthorized();
    });
}

#[test]
fn jwt_with_unknown_session_is_rejected() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let token = sign_session_jwt(user_id(), SessionId::now_v7(), Duration::hours(1));

        let s = server_with(|db| db.append_query_results([Vec::<session::Model>::new()]));

        let res = s
            .get("/api/v1/user")
            .add_header("authorization", format!("Bearer {}", token))
            .await;
        res.assert_status_unauthorized();
    });
}

#[test]
fn jwt_with_expired_session_is_rejected() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
    });
}

#[test]
fn revoked_api_key_is_rejected() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
            permission: gradient_db::permissions::admin_mask(),
            ..Default::default()
        };

        let s = server_with(|db| db.append_query_results([vec![key]]));

        let res = s
            .get("/api/v1/user")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;
        res.assert_status_unauthorized();
    });
}

#[test]
fn expired_api_key_is_rejected() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
            permission: gradient_db::permissions::admin_mask(),
            ..Default::default()
        };

        let s = server_with(|db| db.append_query_results([vec![key]]));

        let res = s
            .get("/api/v1/user")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;
        res.assert_status_unauthorized();
    });
}

/// Sets up the queue an authenticated request reads through:
///   1. session lookup (Query)
///   2. session.last_used_at update (Exec + re-Query)
///   3. user lookup (Query)
fn auth_queue(db: MockDatabase, session: session::Model) -> MockDatabase {
    db.append_query_results([vec![session.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

#[test]
fn delete_user_without_password_is_forbidden() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
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
    });
}

#[test]
fn delete_user_with_wrong_password_is_forbidden() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let session = live_session(SessionId::now_v7());
        let token = sign_session_jwt(user_id(), session.id, Duration::hours(1));

        let s = server_with(|db| auth_queue(db, session));

        let res = s
            .delete("/api/v1/user")
            .add_header("authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({ "password": "WrongPassword!" }))
            .await;
        res.assert_status_forbidden();
    });
}

// ── Configurable API-key options ─────────────────────────────────────────────

#[test]
fn api_key_with_only_view_cannot_trigger_evaluation() {
    use gradient_db::permissions::{Permission, mask_from};
    use gradient_test_support::fixtures::{org, org_id};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let raw = "x".repeat(64);
        let now = Utc::now().naive_utc();
        let key = api::Model {
            id: ApiId::now_v7(),
            owned_by: user_id(),
            name: "ci".into(),
            key: hash_api_key(&raw),
            last_used_at: now,
            created_at: now,
            permission: mask_from(&[Permission::ViewOrg]),
            ..Default::default()
        };
        let admin_membership = gradient_entity::organization_user::Model {
            id: gradient_entity::ids::OrganizationUserId::now_v7(),
            organization: org_id(),
            user: user_id(),
            role: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        };
        let admin_role = gradient_entity::role::Model {
            id: gradient_types::consts::BASE_ROLE_ADMIN_ID,
            name: "Admin".into(),
            permission: gradient_db::permissions::admin_mask(),
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
                .append_query_results([vec![org()]])
                .append_query_results([vec![gradient_entity::project::Model {
                    id: gradient_test_support::fixtures::project_id(),
                    organization: org_id(),
                    name: "test-project".into(),
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
                    sign_cache: true,
                    ..Default::default()
                }]])
                .append_query_results([vec![admin_membership]])
                .append_query_results([vec![admin_role]])
        });

        let res = s
            .post("/api/v1/projects/test-org/test-project/evaluate")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;
        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}

#[test]
fn api_key_pinned_to_other_org_is_invisible() {
    use gradient_db::permissions::{Permission, mask_from};
    use gradient_test_support::fixtures::org;

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let raw = "y".repeat(64);
        let now = Utc::now().naive_utc();
        let pinned_elsewhere =
            gradient_entity::ids::OrganizationId::new(uuid::uuid!("ffffffff-ffff-ffff-ffff-ffffffffffff"));
        let key = api::Model {
            id: ApiId::now_v7(),
            owned_by: user_id(),
            name: "ci".into(),
            key: hash_api_key(&raw),
            last_used_at: now,
            created_at: now,
            permission: mask_from(Permission::ALL),
            organization: Some(pinned_elsewhere),
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
                .append_query_results([vec![org()]])
        });

        let res = s
            .get("/api/v1/orgs/test-org")
            .add_header("authorization", format!("Bearer GRAD{}", raw))
            .await;
        res.assert_status(axum::http::StatusCode::NOT_FOUND);
    });
}

#[test]
fn api_key_cannot_create_api_keys() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let raw = "z".repeat(64);
        let now = Utc::now().naive_utc();
        let key = api::Model {
            id: ApiId::now_v7(),
            owned_by: user_id(),
            name: "self".into(),
            key: hash_api_key(&raw),
            last_used_at: now,
            created_at: now,
            permission: gradient_db::permissions::admin_mask(),
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
                "permissions": ["viewOrg"],
            }))
            .await;
        res.assert_status(axum::http::StatusCode::FORBIDDEN);
    });
}
