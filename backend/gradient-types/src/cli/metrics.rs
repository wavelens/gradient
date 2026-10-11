/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("metrics")]
#[derive(Args, Debug, Clone)]
pub struct MetricsArgs {
    /// Path to a file containing the bearer token required to scrape `/metrics`. An unset path is
    /// disabling the metrics endpoint with 404. The server is reading the file once at startup.
    #[arg(id = "metrics-token-file")]
    pub token_file: Option<String>,

    /// Seconds between metric rollup passes.
    #[arg(default_value_t = 60)]
    pub rollup_interval_secs: u64,

    /// Days to keep raw phase and worker samples and the per-minute cache and upstream traffic
    /// counters. Set it to `0` to keep them forever.
    #[arg(default_value_t = 14)]
    pub retention_raw_days: i64,

    /// Days to keep minute and hour rollups. Day and week rollups stay forever. Set it to `0`
    /// to keep every rollup forever.
    #[arg(default_value_t = 400)]
    pub retention_rollup_days: i64,

    /// Maximum distinct label values per rollup dimension, by activity.
    #[arg(default_value_t = 20)]
    pub label_topn: u32,

    /// Seconds between flushes of cache traffic counters to the database.
    #[arg(default_value_t = 10)]
    pub cache_flush_interval_secs: u64,

    /// Seconds between worker metric samples.
    #[arg(default_value_t = 15)]
    pub worker_sample_interval_secs: u64,

    /// Seconds between updates of the instance-wide metric window.
    #[arg(default_value_t = 30)]
    pub instance_interval_secs: u64,

    /// Seconds between build graph consistency checks. These checks also repair the NAR
    /// reference counter. Set it to `0` to disable both.
    #[arg(default_value_t = 300)]
    pub graph_consistency_interval_secs: u64,

    /// OTLP collector endpoint to push metrics to. Set it to `null` to disable OTLP export.
    pub otlp_endpoint: Option<String>,

    /// Seconds between OTLP metric pushes.
    #[arg(default_value_t = 30)]
    pub otlp_push_interval_secs: u64,
}
