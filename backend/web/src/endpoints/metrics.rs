/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Prometheus exposition endpoint (`GET /metrics`) — closes #35.
//!
//! Collects metrics on demand at scrape time. No background aggregation:
//! one DB query plus one scheduler snapshot per request. The route is only
//! mounted when a metrics token is configured (`MetricsConfig::token`);
//! when absent, callers fall through to the global 404 handler.

use prometheus::{
    Encoder, Gauge, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};

pub(crate) const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

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
        Opts::new("gradient_info", "Build/version metadata; value is always 1."),
        &["version"],
    )
    .expect("metric");
    info.with_label_values(&[&obs.version]).set(1);
    registry.register(Box::new(info)).expect("register info");

    let uptime = Gauge::new("gradient_uptime_seconds", "Seconds since process start.")
        .expect("metric");
    uptime.set(obs.uptime_seconds);
    registry.register(Box::new(uptime)).expect("register uptime");

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

    let workers = IntGauge::new("gradient_workers_connected", "Connected workers.")
        .expect("metric");
    workers.set(obs.workers_connected);
    registry
        .register(Box::new(workers))
        .expect("register workers");

    let pending = IntGauge::new("gradient_jobs_pending", "Pending jobs in scheduler.")
        .expect("metric");
    pending.set(obs.jobs_pending);
    registry
        .register(Box::new(pending))
        .expect("register pending");

    let active = IntGauge::new("gradient_jobs_active", "Active jobs in scheduler.")
        .expect("metric");
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

    let mut buf = Vec::new();
    TextEncoder::new()
        .encode(&registry.gather(), &mut buf)
        .expect("encode");
    String::from_utf8(buf).expect("utf-8")
}

fn register_labelled_counter(
    registry: &Registry,
    name: &str,
    help: &str,
    values: &[(String, i64)],
) {
    let cv = IntCounterVec::new(Opts::new(name, help), &["status"]).expect("metric");
    for (label, value) in values {
        cv.with_label_values(&[label]).inc_by((*value).max(0) as u64);
    }
    registry.register(Box::new(cv)).expect("register");
}

fn register_labelled_gauge(
    registry: &Registry,
    name: &str,
    help: &str,
    values: &[(String, i64)],
) {
    let gv = IntGaugeVec::new(Opts::new(name, help), &["status"]).expect("metric");
    for (label, value) in values {
        gv.with_label_values(&[label]).set(*value);
    }
    registry.register(Box::new(gv)).expect("register");
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
