/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LoggingArgs {
    /// Default log level for the whole binary. Per-component overrides:
    /// `--builder-log-level`, `--cache-log-level`, `--web-log-level`.
    #[arg(long, env = "GRADIENT_LOG_LEVEL", default_value = "info")]
    pub log_level: String,
    /// Log level for the `builder` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_BUILDER_LOG_LEVEL")]
    pub builder_log_level: Option<String>,
    /// Log level for the `cache` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_CACHE_LOG_LEVEL")]
    pub cache_log_level: Option<String>,
    /// Log level for the `web` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_WEB_LOG_LEVEL")]
    pub web_log_level: Option<String>,
    /// Log level for the `proto` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_PROTO_LOG_LEVEL")]
    pub proto_log_level: Option<String>,
}

impl Default for LoggingArgs {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            builder_log_level: None,
            cache_log_level: None,
            web_log_level: None,
            proto_log_level: None,
        }
    }
}
