/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;

use crate::input::port_in_range;
use clap::Args;
use gradient_derive::config_group;

#[config_group]
#[derive(Args, Debug, Clone)]
pub struct ServerArgs {
    /// IP address for the Gradient server to listen on.
    #[arg(default_value = "127.0.0.1")]
    pub listen_addr: String,
    /// Port for the Gradient server to listen on.
    #[arg(value_parser = port_in_range, default_value_t = 3000)]
    pub port: u16,
    /// Public URL under which clients reach Gradient. Set this for a URL other than
    /// {option}`services.gradient.domain`, for example behind a port mapping.
    #[arg(default_value = "http://127.0.0.1:8000")]
    pub serve_url: String,
    /// Public URL of the Gradient frontend, used for links in CI status reports.
    #[arg(default_value = "http://127.0.0.1:8000")]
    pub frontend_url: String,
    /// URL of an image to display instead of the Gradient logo on the sign-in pages, in its
    /// original colors. Set it to `null` to keep the Gradient logo.
    pub frontend_logo_url: Option<String>,
    /// Whether to enable TLS.
    #[arg(default_value = "true")]
    pub use_tls: bool,
    /// Whether to enable advertising HTTP/3 (QUIC) to clients.
    #[arg(default_value = "false")]
    pub use_quic: bool,
    /// Directory holding Gradient's state, NAR files and caches.
    #[arg(default_value = ".")]
    pub base_dir: PathBuf,
    pub store_path: Option<String>,
    /// Expose `GET /api/v1/workers` and worker stats without authentication. Only superusers can
    /// access those endpoints with `false` (default).
    #[arg(default_value = "false")]
    pub public_stats: bool,
    /// Days to keep job assignment records, finished deliveries, worker connection history,
    /// webhook and task action deliveries, expired sessions and CLI logins. Finished admin
    /// tasks, the audit log, per-build resource samples and finished cluster jobs share the same
    /// limit. Removed resource samples no longer feed build predictions. Finished cluster jobs
    /// without remaining members go on the next hourly pass. Active cluster jobs and the newest
    /// finished admin task of each kind stay exempt. Open worker connections stay until the
    /// next connection of the same worker. Set it to `0` to keep every record forever.
    #[arg(default_value_t = 90)]
    pub retention_days: u32,
}

impl ServerArgs {
    /// Where granted worker uploads over `/proto` stage their `*.partial` files.
    pub fn nar_partial_dir(&self) -> std::path::PathBuf {
        self.base_dir.join("nar-partial")
    }

    /// Where chunked HTTP cache uploads stage their `*.partial` files.
    pub fn nar_upload_partial_dir(&self) -> std::path::PathBuf {
        self.base_dir.join("nar-upload-partial")
    }

    /// Where chunked source uploads stage their `*.partial` files.
    pub fn source_upload_partial_dir(&self) -> std::path::PathBuf {
        self.base_dir.join("source-upload-partial")
    }
}
