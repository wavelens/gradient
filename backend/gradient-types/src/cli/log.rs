/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;

use clap::Args;
use gradient_derive::config_group;

#[config_group("log")]
#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Default log level for the whole binary. Per-component overrides are `--log-level-web`,
    /// `--log-level-cache`, `--log-level-proto` and `--log-level-scheduler`. `RUST_LOG` is
    /// overriding everything.
    #[arg(default_value = "info")]
    pub level_default: String,
    /// Log level for the `gradient_web` target. The default is `--log-level-default`.
    pub level_web: Option<String>,
    /// Log level for the `gradient_cache` target. The default is `--log-level-default`.
    pub level_cache: Option<String>,
    /// Log level for the `gradient_proto` target. The default is `--log-level-default`.
    pub level_proto: Option<String>,
    /// Log level for the `gradient_scheduler` target. The default is `--log-level-default`.
    pub level_scheduler: Option<String>,
    /// Target uncompressed size in bytes for each zstd log chunk written on build finalization.
    /// Chunks are splitting on line boundaries. An over-long line may exceed this. The default is
    /// 262144 (256 KiB).
    #[arg(default_value_t = 262144)]
    pub chunk_bytes: usize,
    /// Directory receiving every closed stage span of the server as JSON lines, one file per
    /// process. An unset value is disabling span tracing.
    pub trace_dir: Option<PathBuf>,
}
