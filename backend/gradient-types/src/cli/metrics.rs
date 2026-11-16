/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU64;

use clap::Args;
use gradient_derive::config_group;

#[config_group("metrics")]
#[derive(Args, Debug, Clone)]
pub struct MetricsArgs {
    /// Path to a file containing the bearer token required to scrape `/metrics`. An unset path is
    /// disabling the metrics endpoint with 404. The server reads the file once at startup.
    pub token_file: Option<String>,

    /// Seconds between metric rollup passes.
    #[arg(default_value = "60")]
    pub rollup_interval_secs: NonZeroU64,

    /// Days to keep raw phase and worker samples and the per-minute cache and upstream traffic
    /// counters. Set it to `0` to keep them forever.
    #[arg(default_value_t = 14)]
    pub retention_raw_days: u32,

    /// Days to keep minute and hour rollups. Day and week rollups stay forever. Set it to `0`
    /// to keep every rollup forever.
    #[arg(default_value_t = 400)]
    pub retention_rollup_days: u32,

    /// Maximum distinct label values per rollup dimension, by activity.
    #[arg(default_value_t = 20)]
    pub label_topn: u32,

    /// Seconds between flushes of cache traffic counters to the database.
    #[arg(default_value = "10")]
    pub cache_flush_interval_secs: NonZeroU64,

    /// Seconds between worker metric samples.
    #[arg(default_value = "15")]
    pub worker_sample_interval_secs: NonZeroU64,

    /// Seconds between updates of the instance-wide metric window.
    #[arg(default_value = "30")]
    pub instance_interval_secs: NonZeroU64,

    /// Seconds between build graph consistency checks. These checks also repair the NAR
    /// reference counter. Set it to `0` to disable both.
    #[arg(default_value_t = 300)]
    pub graph_consistency_interval_secs: u64,

    /// OTLP collector endpoint to push metrics to. Set it to `null` to disable OTLP export.
    pub otlp_endpoint: Option<String>,

    /// Seconds between OTLP metric pushes.
    #[arg(default_value = "30")]
    pub otlp_push_interval_secs: NonZeroU64,

    /// OTLP collector endpoint to push traces to. Set it to `null` to disable trace export.
    pub otlp_traces_endpoint: Option<String>,

    /// The service name that OTLP metrics and traces report.
    #[arg(default_value = "gradient")]
    pub otlp_service_name: String,

    /// Ratio of traces to export, from `0.0` (none) to `1.0` (all).
    #[arg(value_parser = parse_otlp_traces_sample_ratio, default_value = "1.0")]
    pub otlp_traces_sample_ratio: f64,
}

fn parse_otlp_traces_sample_ratio(value: &str) -> Result<f64, String> {
    let ratio = value.parse::<f64>().map_err(|error| error.to_string())?;
    if (0.0..=1.0).contains(&ratio) {
        Ok(ratio)
    } else {
        Err("Set the sample ratio of traces to a value between 0.0 and 1.0.".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::{CommandFactory, error::ErrorKind};

    fn parse_ratio(value: &str) -> Result<Option<f64>, ErrorKind> {
        Cli::command()
            .mut_args(|arg| arg.env(None))
            .try_get_matches_from([
                "gradient-server".to_owned(),
                format!("--metrics-otlp-traces-sample-ratio={value}"),
            ])
            .map(|matches| {
                matches
                    .get_one::<f64>("metrics-otlp-traces-sample-ratio")
                    .copied()
            })
            .map_err(|error| error.kind())
    }

    #[test]
    fn rejects_metrics_otlp_traces_sample_ratio_out_of_range() {
        assert_eq!(parse_ratio("1.5"), Err(ErrorKind::ValueValidation));
        assert_eq!(parse_ratio("-0.1"), Err(ErrorKind::ValueValidation));
        assert_eq!(parse_ratio("0.25"), Ok(Some(0.25)));
    }
}
