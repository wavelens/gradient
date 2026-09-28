/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Default log level for the whole binary. Per-component overrides:
    /// `--log-level-web`, `--log-level-cache`, `--log-level-proto`,
    /// `--log-level-scheduler`. `RUST_LOG` overrides everything.
    #[arg(
        long = "log-level-default",
        env = "GRADIENT_LOG_LEVEL_DEFAULT",
        default_value = "info"
    )]
    pub level_default: String,
    /// Log level for the `gradient_web` target. Defaults to `--log-level-default`.
    #[arg(long = "log-level-web", env = "GRADIENT_LOG_LEVEL_WEB")]
    pub level_web: Option<String>,
    /// Log level for the `gradient_cache` target. Defaults to `--log-level-default`.
    #[arg(long = "log-level-cache", env = "GRADIENT_LOG_LEVEL_CACHE")]
    pub level_cache: Option<String>,
    /// Log level for the `gradient_proto` target. Defaults to `--log-level-default`.
    #[arg(long = "log-level-proto", env = "GRADIENT_LOG_LEVEL_PROTO")]
    pub level_proto: Option<String>,
    /// Log level for the `gradient_scheduler` target. Defaults to `--log-level-default`.
    #[arg(long = "log-level-scheduler", env = "GRADIENT_LOG_LEVEL_SCHEDULER")]
    pub level_scheduler: Option<String>,
    /// Target uncompressed size in bytes for each zstd log chunk written when a
    /// build finalizes. Chunks split on line boundaries, so an over-long line
    /// may exceed this. Defaults to 262144 (256 KiB).
    #[arg(
        long = "log-chunk-bytes",
        env = "GRADIENT_LOG_CHUNK_BYTES",
        default_value_t = 262144
    )]
    pub chunk_bytes: usize,
}

impl Default for LogArgs {
    fn default() -> Self {
        Self {
            level_default: "info".into(),
            level_web: None,
            level_cache: None,
            level_proto: None,
            level_scheduler: None,
            chunk_bytes: 262144,
        }
    }
}
