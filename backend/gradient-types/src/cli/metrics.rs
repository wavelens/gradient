/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct MetricsArgs {
    /// Path to a file containing the bearer token required to scrape
    /// `/metrics`. When unset, the metrics endpoint is disabled and
    /// returns 404. The file is read once at startup.
    #[arg(
        id = "metrics-token-file",
        long = "metrics-token-file",
        env = "GRADIENT_METRICS_TOKEN_FILE"
    )]
    pub token_file: Option<String>,

    /// Interval in seconds between metric rollup-aggregator passes.
    #[arg(
        long = "metrics-rollup-interval-secs",
        env = "GRADIENT_METRICS_ROLLUP_INTERVAL_SECS",
        default_value_t = 60
    )]
    pub rollup_interval_secs: u64,

    /// Days to keep raw phase and worker samples and the per-minute cache and
    /// upstream traffic counters. `0` keeps them forever.
    #[arg(
        long = "metrics-retention-raw-days",
        env = "GRADIENT_METRICS_RETENTION_RAW_DAYS",
        default_value_t = 14
    )]
    pub retention_raw_days: i64,

    /// Days to retain minute/hour `metric_rollup` buckets (day/week kept). 0 = keep forever.
    #[arg(
        long = "metrics-retention-rollup-days",
        env = "GRADIENT_METRICS_RETENTION_ROLLUP_DAYS",
        default_value_t = 400
    )]
    pub retention_rollup_days: i64,

    /// Per-dimension cardinality cap for rollup scope labels (top-N by activity).
    #[arg(
        long = "metrics-label-topn",
        env = "GRADIENT_METRICS_LABEL_TOPN",
        default_value_t = 20
    )]
    pub label_topn: u32,

    /// Interval in seconds between flushes of the in-memory cache-traffic
    /// accumulator into `cache_metric`. A flush that fails loses at most this
    /// much traffic telemetry.
    #[arg(
        long = "metrics-cache-flush-interval-secs",
        env = "GRADIENT_METRICS_CACHE_FLUSH_INTERVAL_SECS",
        default_value_t = 10
    )]
    pub cache_flush_interval_secs: u64,

    /// Interval in seconds between worker live-metric samples written to `worker_sample`.
    #[arg(
        long = "metrics-worker-sample-interval-secs",
        env = "GRADIENT_METRICS_WORKER_SAMPLE_INTERVAL_SECS",
        default_value_t = 15
    )]
    pub worker_sample_interval_secs: u64,

    /// Interval in seconds between InstanceContext window recomputations.
    #[arg(
        long = "metrics-instance-interval-secs",
        env = "GRADIENT_METRICS_INSTANCE_INTERVAL_SECS",
        default_value_t = 30
    )]
    pub instance_interval_secs: u64,

    /// Seconds between build graph consistency checks (stale gate flags,
    /// unpromoted builds that can start, unbacked trusted outputs and wedged
    /// Building evaluations are logged as warnings). The check also repairs the
    /// NAR reference counter over the paths pending builds wait on and is that
    /// counter's only backstop; `0` disables both.
    #[arg(
        long = "metrics-graph-consistency-interval-secs",
        env = "GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS",
        default_value_t = 300
    )]
    pub graph_consistency_interval_secs: u64,

    /// OTLP collector endpoint for metric push export. Unset = OTLP disabled.
    #[arg(long = "metrics-otlp-endpoint", env = "GRADIENT_METRICS_OTLP_ENDPOINT")]
    pub otlp_endpoint: Option<String>,

    /// Interval in seconds between OTLP metric push exports.
    #[arg(
        long = "metrics-otlp-push-interval-secs",
        env = "GRADIENT_METRICS_OTLP_PUSH_INTERVAL_SECS",
        default_value_t = 30
    )]
    pub otlp_push_interval_secs: u64,
}

impl Default for MetricsArgs {
    fn default() -> Self {
        Self {
            token_file: None,
            rollup_interval_secs: 60,
            retention_raw_days: 14,
            retention_rollup_days: 400,
            label_topn: 20,
            cache_flush_interval_secs: 10,
            worker_sample_interval_secs: 15,
            instance_interval_secs: 30,
            graph_consistency_interval_secs: 300,
            otlp_endpoint: None,
            otlp_push_interval_secs: 30,
        }
    }
}
