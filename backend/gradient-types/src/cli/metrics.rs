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
    #[arg(long, env = "GRADIENT_METRICS_TOKEN_FILE")]
    pub metrics_token_file: Option<String>,

    /// Interval in seconds between metric rollup-aggregator passes.
    #[arg(long, env = "GRADIENT_METRICS_ROLLUP_INTERVAL", default_value_t = 60)]
    pub metrics_rollup_interval_secs: u64,

    /// Days to retain raw `phase_event` / `worker_sample` rows. 0 = keep forever.
    #[arg(
        long,
        env = "GRADIENT_METRICS_RETENTION_RAW_DAYS",
        default_value_t = 14
    )]
    pub metrics_retention_raw_days: i64,

    /// Days to retain minute/hour `metric_rollup` buckets (day/week kept). 0 = keep forever.
    #[arg(
        long,
        env = "GRADIENT_METRICS_RETENTION_ROLLUP_DAYS",
        default_value_t = 400
    )]
    pub metrics_retention_rollup_days: i64,

    /// Days to retain `dispatched_job` forensic rows. 0 = keep forever.
    #[arg(long, env = "GRADIENT_DISPATCH_RETENTION_DAYS", default_value_t = 30)]
    pub dispatch_retention_days: i64,

    /// Interval in seconds between worker live-metric samples written to `worker_sample`.
    #[arg(long, env = "GRADIENT_WORKER_SAMPLE_INTERVAL", default_value_t = 15)]
    pub worker_sample_interval_secs: u64,

    /// Per-dimension cardinality cap for rollup scope labels (top-N by activity).
    #[arg(long, env = "GRADIENT_METRICS_LABEL_TOPN", default_value_t = 20)]
    pub metrics_label_topn: u32,

    /// OTLP collector endpoint for metric push export. Unset = OTLP disabled.
    #[arg(long, env = "GRADIENT_OTLP_ENDPOINT")]
    pub otlp_endpoint: Option<String>,

    /// Interval in seconds between OTLP metric push exports.
    #[arg(long, env = "GRADIENT_OTLP_PUSH_INTERVAL", default_value_t = 30)]
    pub otlp_push_interval_secs: u64,

    /// Persist runner-up scoring candidates on each `dispatched_job` row.
    #[arg(
        long,
        env = "GRADIENT_DISPATCH_RECORD_CANDIDATES",
        default_value_t = false
    )]
    pub dispatch_record_candidates: bool,

    /// Interval in seconds between InstanceContext window recomputations.
    #[arg(long, env = "GRADIENT_INSTANCE_METRICS_INTERVAL", default_value_t = 30)]
    pub instance_metrics_interval_secs: u64,

    /// Interval in seconds between read-only build-graph consistency sweeps
    /// (stale gate flags, unpromoted-ready anchors, unbacked trusted outputs,
    /// wedged Building evaluations are logged as warnings). 0 disables.
    #[arg(
        long,
        env = "GRADIENT_GRAPH_CONSISTENCY_INTERVAL",
        default_value_t = 300
    )]
    pub graph_consistency_interval_secs: u64,
}

impl Default for MetricsArgs {
    fn default() -> Self {
        Self {
            metrics_token_file: None,
            metrics_rollup_interval_secs: 60,
            metrics_retention_raw_days: 14,
            metrics_retention_rollup_days: 400,
            dispatch_retention_days: 30,
            worker_sample_interval_secs: 15,
            metrics_label_topn: 20,
            otlp_endpoint: None,
            otlp_push_interval_secs: 30,
            dispatch_record_candidates: false,
            instance_metrics_interval_secs: 30,
            graph_consistency_interval_secs: 300,
        }
    }
}
