/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::port_in_range;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ServerArgs {
    #[arg(
        long = "listen-addr",
        env = "GRADIENT_LISTEN_ADDR",
        default_value = "127.0.0.1"
    )]
    pub listen_addr: String,
    #[arg(long = "port", env = "GRADIENT_PORT", value_parser = port_in_range, default_value_t = 3000)]
    pub port: u16,
    #[arg(
        long = "serve-url",
        env = "GRADIENT_SERVE_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    pub serve_url: String,
    /// Public URL of the Gradient frontend, used to build links in CI status
    /// reports (e.g. `https://gradient.example.com`). Defaults to `serve_url`.
    #[arg(
        long = "frontend-url",
        env = "GRADIENT_FRONTEND_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    pub frontend_url: String,
    /// Whether the server is served over TLS (HTTPS). Controls the `Secure`
    /// flag on session cookies. Set to `false` for plain HTTP deployments.
    #[arg(long = "use-tls", env = "GRADIENT_USE_TLS", default_value = "true")]
    pub use_tls: bool,
    /// Advertise HTTP/3 (QUIC) support to connecting clients.
    /// Enabling this does NOT change the backend transport - configure nginx
    /// with `listen 443 quic` and set the `Alt-Svc` header there.
    /// This flag is surfaced via `GET /api/v1/config` so clients can choose
    /// whether to attempt an HTTP/3 upgrade.
    #[arg(long = "use-quic", env = "GRADIENT_USE_QUIC", default_value = "false")]
    pub use_quic: bool,
    #[arg(long = "base-dir", env = "GRADIENT_BASE_DIR", default_value = ".")]
    pub base_dir: String,
    #[arg(long = "store-path", env = "GRADIENT_STORE_PATH")]
    pub store_path: Option<String>,
    /// Expose `GET /api/v1/workers` and worker stats without authentication.
    /// When `false` (default), only superusers can access those endpoints.
    #[arg(
        long = "public-stats",
        env = "GRADIENT_PUBLIC_STATS",
        default_value = "false"
    )]
    pub public_stats: bool,
}

impl ServerArgs {
    /// Where relayed `/proto` uploads stage their `*.partial` files.
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

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1".into(),
            port: 3000,
            serve_url: "http://127.0.0.1:8000".into(),
            frontend_url: "http://127.0.0.1:8000".into(),
            use_tls: true,
            use_quic: false,
            base_dir: ".".into(),
            store_path: None,
            public_stats: false,
        }
    }
}
