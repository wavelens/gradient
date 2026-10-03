/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_notify::EmailSender;
use gradient_storage::NarStore;
use gradient_test_support::cli::test_cli;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_types::{MetricsConfig, RuntimeConfig, SecretString};
use gradient_web::create_router;
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const TOKEN: &str = "metrics-token-abcdef";

/// `sea_orm` is implementing `IntoMockRow` only for entity models and this map type.
fn count_row(kind: &str, status: Option<i32>, value: i64) -> BTreeMap<&'static str, Value> {
    let mut row = BTreeMap::new();
    row.insert("kind", Value::String(Some(kind.to_string())));
    row.insert("status", Value::Int(status));
    row.insert("value", Value::BigInt(Some(value)));
    row
}

fn state_with_metrics(enabled: bool, db: DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let mut runtime = RuntimeConfig::from_cli(&cli).expect("valid test config");
    runtime.metrics = enabled.then(|| MetricsConfig {
        token: TOKEN.to_string(),
    });
    let nar_storage = NarStore::local(&runtime.server.base_dir).expect("nar store");
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: Arc::new(runtime),
        log_storage: Arc::new(NoopLogStorage),
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_util::http::build_client().expect("http"),
        shutdown: gradient_util::shutdown::Shutdown::new(),
        last_used_stamps: gradient_core::last_used_stamps(),
        build_progress: gradient_core::build_progress(),
        eval_progress: gradient_core::eval_progress(),
        cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
        jwt_secret: SecretString::new("test-jwt-secret".into()),
        started_at: chrono::Utc::now(),
        pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
        oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        scim_group_roles: std::sync::Arc::new(Default::default()),
        events: gradient_types::EventBus::default(),
        git_host: gradient_git_host::GitHostRegistry::with_builtin(),
        github_app_install_url: Default::default(),
        upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
        upload_admission: gradient_storage::admission::UploadAdmission::new(
            gradient_storage::admission::Limits {
                concurrency: 16,
                bytes: u64::MAX,
            },
        ),
        delivery_wake: Default::default(),
        eval_assign_wake: Default::default(),
        probe_requests: Default::default(),
        held_evaluations: Default::default(),
        startable_set: Default::default(),
        graph: gradient_core::Graph::stub(),
    })
}

fn empty_db() -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
        .into_connection()
}

#[tokio::test]
async fn endpoint_404_when_no_token_configured() {
    let state = state_with_metrics(false, empty_db());
    let server = TestServer::new(create_router(state).expect("router"));
    let resp = server.get("/metrics").await;
    assert_eq!(resp.status_code(), 404);
}

#[tokio::test]
async fn endpoint_401_when_no_authorization_header() {
    let state = state_with_metrics(true, empty_db());
    let server = TestServer::new(create_router(state).expect("router"));
    let resp = server.get("/metrics").await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn endpoint_401_when_bearer_mismatch() {
    let state = state_with_metrics(true, empty_db());
    let server = TestServer::new(create_router(state).expect("router"));
    let resp = server
        .get("/metrics")
        .add_header("Authorization", "Bearer wrong")
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn endpoint_200_when_bearer_matches() {
    let state = state_with_metrics(true, empty_db());
    let server = TestServer::new(create_router(state).expect("router"));
    let resp = server
        .get("/metrics")
        .add_header("Authorization", &format!("Bearer {TOKEN}"))
        .await;
    assert_eq!(resp.status_code(), 200);

    let ct = resp.header("content-type");
    assert!(
        ct.to_str().unwrap_or("").starts_with("text/plain"),
        "expected text/plain Prometheus content type, got {ct:?}"
    );

    let body = resp.text();
    for needle in [
        "gradient_info",
        "gradient_uptime_seconds",
        "gradient_workers_connected",
        "gradient_jobs_pending",
        "gradient_jobs_active",
        "gradient_cache_bytes",
    ] {
        assert!(body.contains(needle), "missing {needle:?} in:\n{body}");
    }
}

#[tokio::test]
async fn endpoint_reflects_seeded_counts() {
    let rows = vec![
        count_row("build_total", Some(BuildStatus::Completed as i32), 7),
        count_row("build_total", Some(BuildStatus::FailedPermanent as i32), 2),
        count_row("build_in_state", Some(BuildStatus::Queued as i32), 5),
        count_row(
            "evaluation_total",
            Some(EvaluationStatus::Completed as i32),
            3,
        ),
        count_row("cache_bytes", None, 1024),
        count_row("cache_packages", None, 9),
    ];
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection();

    let state = state_with_metrics(true, db);
    let server = TestServer::new(create_router(state).expect("router"));
    let resp = server
        .get("/metrics")
        .add_header("Authorization", &format!("Bearer {TOKEN}"))
        .await;
    assert_eq!(resp.status_code(), 200);

    let body = resp.text();
    for needle in [
        "gradient_builds_total{status=\"Completed\"} 7",
        "gradient_builds_total{status=\"FailedPermanent\"} 2",
        "gradient_builds_in_state{status=\"Queued\"} 5",
        "gradient_evaluations_total{status=\"Completed\"} 3",
        "gradient_cache_bytes 1024",
        "gradient_cache_packages 9",
    ] {
        assert!(body.contains(needle), "missing {needle:?} in:\n{body}");
    }
}

#[tokio::test]
async fn endpoint_rate_limited() {
    // Each successful request is issuing one DB read. The 6th request is throttled before the
    // handler starts.
    let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
    for _ in 0..5 {
        mock = mock.append_query_results([Vec::<BTreeMap<&str, Value>>::new()]);
    }

    let state = state_with_metrics(true, mock.into_connection());
    let server = TestServer::new(create_router(state).expect("router"));

    for i in 1..=5 {
        let r = server
            .get("/metrics")
            .add_header("Authorization", &format!("Bearer {TOKEN}"))
            .await;
        assert_eq!(r.status_code(), 200, "req {i} should succeed");
    }
    let throttled = server
        .get("/metrics")
        .add_header("Authorization", &format!("Bearer {TOKEN}"))
        .await;
    assert_eq!(
        throttled.status_code(),
        429,
        "6th burst request should be 429"
    );
}

#[tokio::test]
async fn endpoint_refills_within_a_second() {
    let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
    for _ in 0..6 {
        mock = mock.append_query_results([Vec::<BTreeMap<&str, Value>>::new()]);
    }

    let state = state_with_metrics(true, mock.into_connection());
    let server = TestServer::new(create_router(state).expect("router"));

    for _ in 1..=5 {
        server
            .get("/metrics")
            .add_header("Authorization", &format!("Bearer {TOKEN}"))
            .await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let refilled = server
        .get("/metrics")
        .add_header("Authorization", &format!("Bearer {TOKEN}"))
        .await;
    assert_eq!(
        refilled.status_code(),
        200,
        "a scraper waiting a second after a burst must not be throttled"
    );
}
