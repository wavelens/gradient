/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Prometheus exposition endpoint (`GET /metrics`) - closes #35.
//!
//! Collects metrics on demand at scrape time. No background aggregation:
//! one DB query plus one scheduler snapshot per request. The route is only
//! mounted when a metrics token is configured (`MetricsConfig::token`);
//! when absent, callers fall through to the global 404 handler.

use std::sync::{Arc, LazyLock};

use axum::extract::{MatchedPath, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use gradient_core::types::ServerState;
use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
    Opts, Registry, TextEncoder,
};
use scheduler::Scheduler;
use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
use subtle::ConstantTimeEq;

use crate::error::{WebError, WebResult};

pub(crate) const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

#[derive(Debug, FromQueryResult)]
struct CountRow {
    kind: String,
    label: Option<String>,
    value: i64,
}

/// Snapshot of values to render. Exposed to the rendering function so unit
/// tests can drive it directly without spinning up a DB or scheduler.
#[derive(Debug, Default)]
pub(crate) struct Observations {
    pub version: String,
    pub uptime_seconds: f64,
    pub builds_total: Vec<(String, i64)>,
    pub builds_in_state: Vec<(String, i64)>,
    pub evaluations_total: Vec<(String, i64)>,
    pub evaluations_in_state: Vec<(String, i64)>,
    pub workers_connected: i64,
    pub jobs_pending: i64,
    pub jobs_active: i64,
    pub cache_bytes: i64,
    pub cache_nar_bytes: i64,
    pub cache_packages: i64,
    pub cache_nar_bytes_sent_total: i64,
    pub cache_nar_requests_total: i64,
}

pub(crate) fn render(obs: &Observations) -> String {
    let registry = Registry::new();

    let info = IntGaugeVec::new(
        Opts::new(
            "gradient_info",
            "Build/version metadata; value is always 1.",
        ),
        &["version"],
    )
    .expect("metric");
    info.with_label_values(&[&obs.version]).set(1);
    registry.register(Box::new(info)).expect("register info");

    let uptime =
        Gauge::new("gradient_uptime_seconds", "Seconds since process start.").expect("metric");
    uptime.set(obs.uptime_seconds);
    registry
        .register(Box::new(uptime))
        .expect("register uptime");

    register_labelled_counter(
        &registry,
        "gradient_builds_total",
        "Total builds that have reached a terminal status, by status.",
        &obs.builds_total,
    );
    register_labelled_gauge(
        &registry,
        "gradient_builds_in_state",
        "Current count of non-terminal builds, by status.",
        &obs.builds_in_state,
    );
    register_labelled_counter(
        &registry,
        "gradient_evaluations_total",
        "Total evaluations that have reached a terminal status, by status.",
        &obs.evaluations_total,
    );
    register_labelled_gauge(
        &registry,
        "gradient_evaluations_in_state",
        "Current count of non-terminal evaluations, by status.",
        &obs.evaluations_in_state,
    );

    let workers =
        IntGauge::new("gradient_workers_connected", "Connected workers.").expect("metric");
    workers.set(obs.workers_connected);
    registry
        .register(Box::new(workers))
        .expect("register workers");

    let pending =
        IntGauge::new("gradient_jobs_pending", "Pending jobs in scheduler.").expect("metric");
    pending.set(obs.jobs_pending);
    registry
        .register(Box::new(pending))
        .expect("register pending");

    let active =
        IntGauge::new("gradient_jobs_active", "Active jobs in scheduler.").expect("metric");
    active.set(obs.jobs_active);
    registry
        .register(Box::new(active))
        .expect("register active");

    let bytes = IntGauge::new(
        "gradient_cache_bytes",
        "Total compressed bytes of all cached NARs.",
    )
    .expect("metric");
    bytes.set(obs.cache_bytes);
    registry.register(Box::new(bytes)).expect("register bytes");

    let nar_bytes = IntGauge::new(
        "gradient_cache_nar_bytes",
        "Total uncompressed NAR bytes of all cached packages.",
    )
    .expect("metric");
    nar_bytes.set(obs.cache_nar_bytes);
    registry
        .register(Box::new(nar_bytes))
        .expect("register nar_bytes");

    let pkgs = IntGauge::new(
        "gradient_cache_packages",
        "Total packages (signed build outputs) in caches.",
    )
    .expect("metric");
    pkgs.set(obs.cache_packages);
    registry
        .register(Box::new(pkgs))
        .expect("register packages");

    let bytes_sent = IntCounter::new(
        "gradient_cache_nar_bytes_sent_total",
        "Total compressed bytes served from the NAR cache since first traffic record.",
    )
    .expect("metric");
    bytes_sent.inc_by(obs.cache_nar_bytes_sent_total.max(0) as u64);
    registry
        .register(Box::new(bytes_sent))
        .expect("register bytes_sent");

    let reqs = IntCounter::new(
        "gradient_cache_nar_requests_total",
        "Total NAR requests served since first traffic record.",
    )
    .expect("metric");
    reqs.inc_by(obs.cache_nar_requests_total.max(0) as u64);
    registry.register(Box::new(reqs)).expect("register reqs");

    // Process/runtime metrics (RSS, open fds, CPU) on Linux via the prometheus
    // process collector. No-op on other platforms.
    #[cfg(target_os = "linux")]
    {
        let pc = prometheus::process_collector::ProcessCollector::for_self();
        let _ = registry.register(Box::new(pc));
    }

    let mut buf = Vec::new();
    TextEncoder::new()
        .encode(&registry.gather(), &mut buf)
        .expect("encode");
    String::from_utf8(buf).expect("utf-8")
}

/// Persistent per-route HTTP metrics, accumulated across requests by
/// [`track_http_metrics`] and merged into the scrape output (#212).
struct HttpMetrics {
    registry: Registry,
    duration: HistogramVec,
    requests: IntCounterVec,
}

static HTTP_METRICS: LazyLock<HttpMetrics> = LazyLock::new(|| {
    let registry = Registry::new();
    let duration = HistogramVec::new(
        HistogramOpts::new(
            "gradient_http_request_duration_seconds",
            "HTTP request duration in seconds by route.",
        )
        .buckets(vec![
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ]),
        &["method", "route"],
    )
    .expect("metric");
    registry
        .register(Box::new(duration.clone()))
        .expect("register http duration");
    let requests = IntCounterVec::new(
        Opts::new(
            "gradient_http_requests_total",
            "HTTP requests by route, method, and status.",
        ),
        &["method", "route", "status"],
    )
    .expect("metric");
    registry
        .register(Box::new(requests.clone()))
        .expect("register http requests");
    HttpMetrics { registry, duration, requests }
});

fn gather_http() -> String {
    let mut buf = Vec::new();
    let _ = TextEncoder::new().encode(&HTTP_METRICS.registry.gather(), &mut buf);
    String::from_utf8(buf).unwrap_or_default()
}

/// Middleware recording each request's duration and status keyed by the matched
/// route template (so dynamic segments don't explode label cardinality).
pub async fn track_http_metrics(request: axum::extract::Request, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let start = std::time::Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16().to_string();
    HTTP_METRICS
        .duration
        .with_label_values(&[&method, &route])
        .observe(start.elapsed().as_secs_f64());
    HTTP_METRICS
        .requests
        .with_label_values(&[&method, &route, &status])
        .inc();
    response
}

fn register_labelled_counter(
    registry: &Registry,
    name: &str,
    help: &str,
    values: &[(String, i64)],
) {
    let cv = IntCounterVec::new(Opts::new(name, help), &["status"]).expect("metric");
    for (label, value) in values {
        cv.with_label_values(&[label])
            .inc_by((*value).max(0) as u64);
    }
    registry.register(Box::new(cv)).expect("register");
}

fn register_labelled_gauge(registry: &Registry, name: &str, help: &str, values: &[(String, i64)]) {
    let gv = IntGaugeVec::new(Opts::new(name, help), &["status"]).expect("metric");
    for (label, value) in values {
        gv.with_label_values(&[label]).set(*value);
    }
    registry.register(Box::new(gv)).expect("register");
}

/// Collect metrics by querying the DB and scheduler in-memory state.
///
/// Errors propagate as `WebError`; the handler converts those into 500.
/// We intentionally never serve a partial response - Prometheus would
/// treat a 200 with missing series as authoritative and corrupt counters.
pub(crate) async fn collect(
    state: &Arc<ServerState>,
    scheduler: &Scheduler,
) -> WebResult<Observations> {
    // Single CTE-style query returning typed rows for every counter we need.
    // Statuses are mapped to text via CASE so values survive numeric reshuffles
    // in `BuildStatus` / `EvaluationStatus` ordering.
    let sql = r#"
        SELECT 'build_total'::text AS kind,
               CASE status
                 WHEN 3 THEN 'Completed'
                 WHEN 4 THEN 'Failed'
                 WHEN 5 THEN 'Aborted'
                 WHEN 6 THEN 'DependencyFailed'
                 WHEN 7 THEN 'Substituted'
                 ELSE NULL
               END AS label,
               COUNT(*)::bigint AS value
        FROM build
        WHERE status IN (3,4,5,6,7)
        GROUP BY status

        UNION ALL

        SELECT 'build_in_state'::text,
               CASE status
                 WHEN 0 THEN 'Created'
                 WHEN 1 THEN 'Queued'
                 WHEN 2 THEN 'Building'
                 ELSE NULL
               END,
               COUNT(*)::bigint
        FROM build
        WHERE status IN (0,1,2)
        GROUP BY status

        UNION ALL

        SELECT 'evaluation_total'::text,
               CASE status
                 WHEN 5 THEN 'Completed'
                 WHEN 6 THEN 'Failed'
                 WHEN 7 THEN 'Aborted'
                 ELSE NULL
               END,
               COUNT(*)::bigint
        FROM evaluation
        WHERE status IN (5,6,7)
        GROUP BY status

        UNION ALL

        SELECT 'evaluation_in_state'::text,
               CASE status
                 WHEN 0 THEN 'Queued'
                 WHEN 1 THEN 'EvaluatingFlake'
                 WHEN 2 THEN 'EvaluatingDerivation'
                 WHEN 3 THEN 'Building'
                 WHEN 4 THEN 'Waiting'
                 WHEN 8 THEN 'Fetching'
                 ELSE NULL
               END,
               COUNT(*)::bigint
        FROM evaluation
        WHERE status IN (0,1,2,3,4,8)
        GROUP BY status

        UNION ALL

        SELECT 'cache_bytes'::text, NULL::text, COALESCE(SUM(file_size), 0)::bigint
        FROM cached_path

        UNION ALL

        SELECT 'cache_nar_bytes'::text, NULL::text, COALESCE(SUM(nar_size), 0)::bigint
        FROM cached_path

        UNION ALL

        SELECT 'cache_packages'::text, NULL::text, COUNT(*)::bigint
        FROM cached_path_signature

        UNION ALL

        SELECT 'cache_nar_bytes_sent_total'::text, NULL::text,
               COALESCE(SUM(bytes_sent), 0)::bigint
        FROM cache_metric

        UNION ALL

        SELECT 'cache_nar_requests_total'::text, NULL::text,
               COALESCE(SUM(nar_count)::bigint, 0)
        FROM cache_metric
    "#;

    let rows: Vec<CountRow> =
        CountRow::find_by_statement(Statement::from_string(DatabaseBackend::Postgres, sql))
            .all(&state.web_db)
            .await
            .map_err(WebError::from)?;

    let mut obs = Observations {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: (Utc::now() - state.started_at).num_milliseconds() as f64 / 1000.0,
        ..Default::default()
    };

    for row in rows {
        match row.kind.as_str() {
            "build_total" => {
                if let Some(l) = row.label {
                    obs.builds_total.push((l, row.value));
                }
            }
            "build_in_state" => {
                if let Some(l) = row.label {
                    obs.builds_in_state.push((l, row.value));
                }
            }
            "evaluation_total" => {
                if let Some(l) = row.label {
                    obs.evaluations_total.push((l, row.value));
                }
            }
            "evaluation_in_state" => {
                if let Some(l) = row.label {
                    obs.evaluations_in_state.push((l, row.value));
                }
            }
            "cache_bytes" => obs.cache_bytes = row.value,
            "cache_nar_bytes" => obs.cache_nar_bytes = row.value,
            "cache_packages" => obs.cache_packages = row.value,
            "cache_nar_bytes_sent_total" => obs.cache_nar_bytes_sent_total = row.value,
            "cache_nar_requests_total" => obs.cache_nar_requests_total = row.value,
            _ => {}
        }
    }

    let (workers, pending, active) = scheduler.metrics_snapshot().await;
    obs.workers_connected = workers as i64;
    obs.jobs_pending = pending as i64;
    obs.jobs_active = active as i64;

    Ok(obs)
}

/// Per-route middleware enforcing the bearer token configured via
/// `MetricsConfig`. The route is only mounted when `state.config.metrics`
/// is `Some`, so unwrapping is invariant-safe inside the closure.
pub async fn metrics_auth(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(cfg) = state.config.metrics.as_ref() else {
        // Defensive: the route shouldn't be reachable without a config,
        // but a 404 here keeps behavior consistent with the unmounted case.
        return StatusCode::NOT_FOUND.into_response();
    };

    let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let Some(presented) = value.strip_prefix("Bearer ") else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let presented_bytes = presented.as_bytes();
    let token_bytes = cfg.token.as_bytes();

    // Length check before constant-time compare: ConstantTimeEq's contract
    // requires equal-length slices for a meaningful result. Token length
    // is operator-controlled, not user-controlled, so the early return
    // does not leak secret material.
    if presented_bytes.len() != token_bytes.len()
        || presented_bytes.ct_eq(token_bytes).unwrap_u8() != 1
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    next.run(request).await
}

pub async fn get_metrics(
    State(state): State<Arc<ServerState>>,
    axum::Extension(scheduler): axum::Extension<Arc<Scheduler>>,
) -> Response {
    match collect(&state, &scheduler).await {
        Ok(obs) => {
            let body = format!("{}{}", render(&obs), gather_http());
            (
                StatusCode::OK,
                [(CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
                body,
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "metrics collection failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_emits_expected_metric_names_and_help() {
        let obs = Observations {
            version: "1.2.3".into(),
            uptime_seconds: 42.5,
            builds_total: vec![("Completed".into(), 7), ("Failed".into(), 2)],
            builds_in_state: vec![("Queued".into(), 3)],
            evaluations_total: vec![("Completed".into(), 5)],
            evaluations_in_state: vec![("Building".into(), 1)],
            workers_connected: 4,
            jobs_pending: 6,
            jobs_active: 2,
            cache_bytes: 1024,
            cache_nar_bytes: 2048,
            cache_packages: 9,
            cache_nar_bytes_sent_total: 999,
            cache_nar_requests_total: 11,
        };

        let body = render(&obs);

        for needle in [
            "# HELP gradient_info",
            "# TYPE gradient_info gauge",
            "gradient_info{version=\"1.2.3\"} 1",
            "# TYPE gradient_uptime_seconds gauge",
            "gradient_uptime_seconds 42.5",
            "# TYPE gradient_builds_total counter",
            "gradient_builds_total{status=\"Completed\"} 7",
            "gradient_builds_total{status=\"Failed\"} 2",
            "gradient_builds_in_state{status=\"Queued\"} 3",
            "gradient_evaluations_total{status=\"Completed\"} 5",
            "gradient_evaluations_in_state{status=\"Building\"} 1",
            "gradient_workers_connected 4",
            "gradient_jobs_pending 6",
            "gradient_jobs_active 2",
            "gradient_cache_bytes 1024",
            "gradient_cache_nar_bytes 2048",
            "gradient_cache_packages 9",
            "gradient_cache_nar_bytes_sent_total 999",
            "gradient_cache_nar_requests_total 11",
        ] {
            assert!(body.contains(needle), "missing {needle:?} in:\n{body}");
        }
    }
}
