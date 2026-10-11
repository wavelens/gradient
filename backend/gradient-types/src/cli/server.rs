/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::port_in_range;
use clap::Args;
use gradient_derive::config_group;

#[config_group]
#[derive(Args, Debug, Clone)]
pub struct ServerArgs {
    #[arg(default_value = "127.0.0.1")]
    pub listen_addr: String,
    #[arg(value_parser = port_in_range, default_value_t = 3000)]
    pub port: u16,
    #[arg(default_value = "http://127.0.0.1:8000")]
    pub serve_url: String,
    /// Public URL of the Gradient frontend, used for links in CI status reports.
    #[arg(default_value = "http://127.0.0.1:8000")]
    pub frontend_url: String,
    pub frontend_logo_url: Option<String>,
    /// Whether the server is served over TLS (HTTPS). It is controlling the `Secure` flag on
    /// session cookies. Set it to `false` for plain HTTP deployments.
    #[arg(default_value = "true")]
    pub use_tls: bool,
    /// Advertise HTTP/3 (QUIC) support to connecting clients. Enabling this is NOT changing the
    /// backend transport. Configure nginx with `listen 443 quic` and set the `Alt-Svc` header
    /// there. `GET /api/v1/config` is surfacing this flag for clients choosing whether to attempt
    /// an HTTP/3 upgrade.
    #[arg(default_value = "false")]
    pub use_quic: bool,
    #[arg(default_value = ".")]
    pub base_dir: String,
    pub store_path: Option<String>,
    /// Expose `GET /api/v1/workers` and worker stats without authentication. Only superusers can
    /// access those endpoints with `false` (default).
    #[arg(default_value = "false")]
    pub public_stats: bool,
    /// Days to keep job assignment records, finished deliveries, worker connection history, webhook
    /// and task action deliveries, expired sessions and CLI logins. The same limit is covering
    /// finished admin tasks, the audit log, per-build resource samples and finished cluster jobs.
    /// Pruned resource samples are no longer feeding build predictions. A finished cluster job
    /// without remaining members is going on the next hourly pass. The pruning is sparing the
    /// newest finished admin task of each kind and active cluster jobs. An open worker connection
    /// is kept until the same worker is connecting again. `0` is keeping every record forever.
    #[arg(default_value_t = 90)]
    pub retention_days: i64,
}

impl ServerArgs {
    /// Where granted worker uploads over `/proto` stage their `*.partial` files.
    pub fn nar_partial_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.base_dir).join("nar-partial")
    }

    /// Where chunked HTTP cache uploads stage their `*.partial` files.
    pub fn nar_upload_partial_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.base_dir).join("nar-upload-partial")
    }

    /// Where chunked source uploads stage their `*.partial` files.
    pub fn source_upload_partial_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.base_dir).join("source-upload-partial")
    }
}
