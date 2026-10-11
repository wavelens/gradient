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
    /// Target uncompressed size in bytes of a stored build log chunk. Chunks split on line
    /// boundaries, and a long line can exceed the target.
    #[arg(default_value_t = 262144)]
    pub chunk_bytes: usize,
    /// Directory receiving every closed stage span of the server as JSON lines, one file per
    /// process. Set it to `null` to disable span tracing.
    pub trace_dir: Option<PathBuf>,
}
