/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::port_in_range;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ServerArgs {
    #[arg(long, env = "GRADIENT_IP", default_value = "127.0.0.1")]
    pub ip: String,
    #[arg(long, env = "GRADIENT_PORT", value_parser = port_in_range, default_value_t = 3000)]
    pub port: u16,
    #[arg(
        long,
        env = "GRADIENT_SERVE_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    pub serve_url: String,
    /// Public URL of the Gradient frontend, used to build links in CI status
    /// reports (e.g. `https://gradient.example.com`). Defaults to `serve_url`.
    #[arg(
        long,
        env = "GRADIENT_FRONTEND_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    pub frontend_url: String,
    /// Whether the server is served over TLS (HTTPS). Controls the `Secure`
    /// flag on session cookies. Set to `false` for plain HTTP deployments.
    #[arg(long, env = "GRADIENT_USE_TLS", default_value = "true")]
    pub use_tls: bool,
}

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            ip: "127.0.0.1".into(),
            port: 3000,
            serve_url: "http://127.0.0.1:8000".into(),
            frontend_url: "http://127.0.0.1:8000".into(),
            use_tls: true,
        }
    }
}
