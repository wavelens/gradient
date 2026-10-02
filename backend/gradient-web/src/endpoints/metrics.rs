/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::{Arc, LazyLock};

use axum::extract::{MatchedPath, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_scheduler::Scheduler;
use gradient_util::metrics::{
    PROMETHEUS_CONTENT_TYPE, encode_text, register_labelled_counter, register_labelled_gauge,
    register_process_collector,
};
use prometheus::{
    Counter, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
    Opts, Registry,
};
use sea_orm::{FromQueryResult, Iterable};
use subtle::ConstantTimeEq;

use crate::error::{WebError, WebResult};

#[derive(Debug, FromQueryResult)]
struct CountRow {
    kind: String,
    status: Option<i32>,
    value: i64,
}

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
    pub uploads: gradient_storage::admission::AdmissionStats,
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
        "status",
        &obs.builds_total,
    )
    .expect("register");

    register_labelled_gauge(
        &registry,
        "gradient_builds_in_state",
        "Current count of non-terminal builds, by status.",
        "status",
        &obs.builds_in_state,
    )
    .expect("register");

    register_labelled_counter(
        &registry,
        "gradient_evaluations_total",
        "Total evaluations that have reached a terminal status, by status.",
        "status",
        &obs.evaluations_total,
    )
    .expect("register");

    register_labelled_gauge(
        &registry,
        "gradient_evaluations_in_state",
        "Current count of non-terminal evaluations, by status.",
        "status",
        &obs.evaluations_in_state,
    )
    .expect("register");

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

    render_uploads(&registry, &obs.uploads);

    register_process_collector(&registry);
    encode_text(&registry)
}

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

    HttpMetrics {
        registry,
        duration,
        requests,
    }
});

fn gather_http() -> String {
    encode_text(&HTTP_METRICS.registry)
}

#[derive(serde::Serialize)]
pub struct HttpRouteStat {
    pub method: String,
    pub route: String,
    pub count: u64,
    pub avg_ms: f64,
    pub errors: u64,
}

pub(crate) fn http_snapshot() -> Vec<HttpRouteStat> {
    use std::collections::HashMap;
    let mut dur: HashMap<(String, String), (u64, f64)> = HashMap::new();
    let mut req: HashMap<(String, String), (u64, u64)> = HashMap::new();
    for mf in HTTP_METRICS.registry.gather() {
        let label = |m: &prometheus::proto::Metric, key: &str| {
            m.get_label()
                .iter()
                .find(|l| l.name() == key)
                .map(|l| l.value().to_owned())
                .unwrap_or_default()
        };

        match mf.name() {
            "gradient_http_request_duration_seconds" => {
                for m in mf.get_metric() {
                    let h = m.get_histogram();
                    dur.insert(
                        (label(m, "method"), label(m, "route")),
                        (h.get_sample_count(), h.get_sample_sum()),
                    );
                }
            }
            "gradient_http_requests_total" => {
                for m in mf.get_metric() {
                    let status = label(m, "status");
                    let v = m.get_counter().get_value() as u64;
                    let e = req
                        .entry((label(m, "method"), label(m, "route")))
                        .or_insert((0, 0));
                    e.0 += v;
                    if status.starts_with('4') || status.starts_with('5') {
                        e.1 += v;
                    }
                }
            }
            _ => {}
        }
    }

    let mut out: Vec<HttpRouteStat> = dur
        .into_iter()
        .map(|((method, route), (count, sum))| {
            let (total, errors) = req
                .get(&(method.clone(), route.clone()))
                .copied()
                .unwrap_or((count, 0));
            HttpRouteStat {
                method,
                route,
                count: total.max(count),
                avg_ms: if count > 0 {
                    sum / count as f64 * 1000.0
                } else {
                    0.0
                },
                errors,
            }
        })
        .collect();

    out.sort_by_key(|s| std::cmp::Reverse(s.count));
    out
}

#[derive(serde::Serialize, Default)]
pub struct ProcessStat {
    pub resident_memory_bytes: f64,
    pub virtual_memory_bytes: f64,
    pub open_fds: f64,
    pub max_fds: f64,
    pub cpu_seconds_total: f64,
    pub threads: f64,
}

pub(crate) fn process_snapshot() -> ProcessStat {
    let mut s = ProcessStat::default();
    #[cfg(target_os = "linux")]
    {
        let registry = Registry::new();
        let pc = prometheus::process_collector::ProcessCollector::for_self();
        let _ = registry.register(Box::new(pc));
        for mf in registry.gather() {
            let Some(m) = mf.get_metric().first() else {
                continue;
            };
            let val = if mf.name() == "process_cpu_seconds_total" {
                m.get_counter().get_value()
            } else {
                m.get_gauge().get_value()
            };

            match mf.name() {
                "process_resident_memory_bytes" => s.resident_memory_bytes = val,
                "process_virtual_memory_bytes" => s.virtual_memory_bytes = val,
                "process_open_fds" => s.open_fds = val,
                "process_max_fds" => s.max_fds = val,
                "process_cpu_seconds_total" => s.cpu_seconds_total = val,
                "process_threads" => s.threads = val,
                _ => {}
            }
        }
    }
    s
}

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

// Status sets and label names are coming from the enums. A new or renumbered variant can never
// silently vanish from a series.
fn observations_sql() -> String {
    let build_terminal: Vec<BuildStatus> = BuildStatus::iter()
        .filter(|s| {
            s.is_terminal_success() || s.is_terminal_failure() || *s == BuildStatus::Aborted
        })
        .collect();
    format!(
        r#"
        SELECT CASE WHEN status IN ({build_terminal}) THEN 'build_total'
                    ELSE 'build_in_state' END::text AS kind,
               status::int AS status,
               COUNT(*)::bigint AS value
        FROM derivation_build
        GROUP BY status

        UNION ALL

        SELECT CASE WHEN status IN ({eval_terminal}) THEN 'evaluation_total'
                    ELSE 'evaluation_in_state' END::text,
               status::int,
               COUNT(*)::bigint
        FROM evaluation
        GROUP BY status

        UNION ALL

        SELECT 'cache_bytes'::text, NULL::int, COALESCE(SUM(file_size), 0)::bigint
        FROM cached_path

        UNION ALL

        SELECT 'cache_nar_bytes'::text, NULL::int, COALESCE(SUM(nar_size), 0)::bigint
        FROM cached_path

        UNION ALL

        SELECT 'cache_packages'::text, NULL::int, COUNT(*)::bigint
        FROM cached_path_signature

        UNION ALL

        SELECT 'cache_nar_bytes_sent_total'::text, NULL::int,
               COALESCE(SUM(bytes_sent), 0)::bigint
        FROM cache_metric

        UNION ALL

        SELECT 'cache_nar_requests_total'::text, NULL::int,
               COALESCE(SUM(nar_count)::bigint, 0)
        FROM cache_metric
    "#,
        build_terminal = gradient_db::sql::status::build_in(&build_terminal),
        eval_terminal = gradient_db::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

gradient_db::sql_fn! {
    OBSERVATIONS = observations_sql,
        params = [],
        tier = Bulk;
}

fn render_uploads(registry: &Registry, uploads: &gradient_storage::admission::AdmissionStats) {
    let in_flight =
        IntGauge::new("gradient_upload_in_flight", "Uploads holding a permit.").expect("metric");
    in_flight.set(uploads.in_flight as i64);
    registry
        .register(Box::new(in_flight))
        .expect("register upload in flight");

    let bytes = IntGauge::new(
        "gradient_upload_bytes_in_flight",
        "Bytes of admitted uploads.",
    )
    .expect("metric");
    bytes.set(uploads.bytes_in_flight as i64);
    registry
        .register(Box::new(bytes))
        .expect("register upload bytes");

    let depth = IntGaugeVec::new(
        Opts::new(
            "gradient_upload_queue_depth",
            "Upload requests waiting for a permit.",
        ),
        &["worker"],
    )
    .expect("metric");
    for (worker, n) in &uploads.queued {
        depth.with_label_values(&[worker]).set(*n as i64);
    }
    registry
        .register(Box::new(depth))
        .expect("register upload queue depth");

    let granted = IntCounter::new("gradient_upload_granted_total", "Upload permits granted.")
        .expect("metric");
    granted.inc_by(uploads.granted_total);
    registry
        .register(Box::new(granted))
        .expect("register upload grants");

    let waited = Counter::new(
        "gradient_upload_wait_seconds_total",
        "Seconds uploads waited for a permit.",
    )
    .expect("metric");
    waited.inc_by(uploads.wait_seconds_total);
    registry
        .register(Box::new(waited))
        .expect("register upload wait");
}

pub(crate) async fn collect(
    state: &Arc<ServerState>,
    scheduler: &Scheduler,
) -> WebResult<Observations> {
    let rows: Vec<CountRow> = CountRow::find_by_statement(OBSERVATIONS.stmt())
        .all(&state.web_db)
        .await
        .map_err(WebError::from)?;

    let mut obs = Observations {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: (Utc::now() - state.started_at).num_milliseconds() as f64 / 1000.0,
        uploads: state.upload_admission.stats(),
        ..Default::default()
    };

    let build_label = |s: Option<i32>| {
        s.and_then(|n| BuildStatus::try_from(n).ok())
            .map(|v| format!("{v:?}"))
    };
    let eval_label = |s: Option<i32>| {
        s.and_then(|n| EvaluationStatus::try_from(n).ok())
            .map(|v| format!("{v:?}"))
    };
    for row in rows {
        match row.kind.as_str() {
            "build_total" => {
                if let Some(l) = build_label(row.status) {
                    obs.builds_total.push((l, row.value));
                }
            }
            "build_in_state" => {
                if let Some(l) = build_label(row.status) {
                    obs.builds_in_state.push((l, row.value));
                }
            }
            "evaluation_total" => {
                if let Some(l) = eval_label(row.status) {
                    obs.evaluations_total.push((l, row.value));
                }
            }
            "evaluation_in_state" => {
                if let Some(l) = eval_label(row.status) {
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

pub async fn metrics_auth(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(cfg) = state.config.metrics.as_ref() else {
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

    // The length check is preceding the constant-time compare because `ConstantTimeEq` is only
    // meaningful on equal-length slices. Token length is operator-controlled, and the early return
    // is leaking no secret.
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
            uploads: gradient_storage::admission::AdmissionStats {
                in_flight: 3,
                bytes_in_flight: 4096,
                queued: vec![("w1".into(), 5)],
                granted_total: 8,
                wait_seconds_total: 1.5,
            },
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
            "gradient_upload_in_flight 3",
            "gradient_upload_bytes_in_flight 4096",
            "gradient_upload_queue_depth{worker=\"w1\"} 5",
            "gradient_upload_granted_total 8",
            "gradient_upload_wait_seconds_total 1.5",
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
