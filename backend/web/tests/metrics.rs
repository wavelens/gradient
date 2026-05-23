/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `GET /metrics` (issue #35).
//!
//! Each test builds a `ServerState` directly so it can inject a `MockDatabase`
//! pre-staged with the row set the metrics collector expects. The metrics
//! handler issues exactly one DB query per request, so we stage exactly one
//! result set per expected scrape.

use axum_test::TestServer;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::{
    MetricsConfig, RuntimeConfig, SecretString, ServerState, WebDb, WorkerDb,
};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use test_support::cli::test_cli;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::log_storage::NoopLogStorage;
use web::create_router;

const TOKEN: &str = "metrics-token-abcdef";

/// Build a mock row matching the `CountRow { kind, label, value }` shape
/// that the metrics collector queries for. We construct rows as
/// `BTreeMap<&str, Value>` because `sea_orm` only implements `IntoMockRow`
/// for entity models (which we don't want to fabricate here) and for that
/// map type.
fn count_row(kind: &str, label: Option<&str>, value: i64) -> BTreeMap<&'static str, Value> {
    let mut row = BTreeMap::new();
    row.insert("kind", Value::String(Some(Box::new(kind.to_string()))));
    row.insert(
        "label",
        Value::String(label.map(|l| Box::new(l.to_string()))),
    );
    row.insert("value", Value::BigInt(Some(value)));
    row
}

fn state_with_metrics(enabled: bool, db: DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let mut runtime = RuntimeConfig::from_cli(&cli).expect("valid test config");
    runtime.metrics = enabled.then(|| MetricsConfig {
        token: TOKEN.to_string(),
    });
    let nar_storage = NarStore::local(&runtime.storage.base_path).expect("nar store");
    Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config: Arc::new(runtime),
        log_storage: Arc::new(NoopLogStorage),        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".into()),
        started_at: chrono::Utc::now(),
    })
}

fn empty_db() -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
        .into_connection()
}

#[test]
fn endpoint_404_when_no_token_configured() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = state_with_metrics(false, empty_db());
        let server = TestServer::new(create_router(state));
        let resp = server.get("/metrics").await;
        assert_eq!(resp.status_code(), 404);
    });
}

#[test]
fn endpoint_401_when_no_authorization_header() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = state_with_metrics(true, empty_db());
        let server = TestServer::new(create_router(state));
        let resp = server.get("/metrics").await;
        assert_eq!(resp.status_code(), 401);
    });
}

#[test]
fn endpoint_401_when_bearer_mismatch() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = state_with_metrics(true, empty_db());
        let server = TestServer::new(create_router(state));
        let resp = server
            .get("/metrics")
            .add_header("Authorization", "Bearer wrong")
            .await;
        assert_eq!(resp.status_code(), 401);
    });
}

#[test]
fn endpoint_200_when_bearer_matches() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let state = state_with_metrics(true, empty_db());
        let server = TestServer::new(create_router(state));
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
    });
}

#[test]
fn endpoint_reflects_seeded_counts() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let rows = vec![
            count_row("build_total", Some("Completed"), 7),
            count_row("build_total", Some("Failed"), 2),
            count_row("build_in_state", Some("Queued"), 5),
            count_row("evaluation_total", Some("Completed"), 3),
            count_row("cache_bytes", None, 1024),
            count_row("cache_packages", None, 9),
        ];
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows])
            .into_connection();

        let state = state_with_metrics(true, db);
        let server = TestServer::new(create_router(state));
        let resp = server
            .get("/metrics")
            .add_header("Authorization", &format!("Bearer {TOKEN}"))
            .await;
        assert_eq!(resp.status_code(), 200);

        let body = resp.text();
        for needle in [
            "gradient_builds_total{status=\"Completed\"} 7",
            "gradient_builds_total{status=\"Failed\"} 2",
            "gradient_builds_in_state{status=\"Queued\"} 5",
            "gradient_evaluations_total{status=\"Completed\"} 3",
            "gradient_cache_bytes 1024",
            "gradient_cache_packages 9",
        ] {
            assert!(body.contains(needle), "missing {needle:?} in:\n{body}");
        }
    });
}

#[test]
fn endpoint_rate_limited() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Each successful request issues one DB read; pre-stage 5 empty
        // result sets (the 6th is throttled before the handler runs).
        let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
        for _ in 0..5 {
            mock = mock.append_query_results([Vec::<BTreeMap<&str, Value>>::new()]);
        }

        let state = state_with_metrics(true, mock.into_connection());
        let server = TestServer::new(create_router(state));

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
    });
}
