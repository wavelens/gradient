/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::port_in_range;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

/// Who may create organizations / caches through the API.
#[derive(ValueEnum, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum CreatePermission {
    /// Nobody via the API; only the declarative state import may create them.
    None,
    /// Superusers only.
    Superusers,
    /// Any authenticated user.
    #[default]
    Everyone,
}

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
    /// Author/committer name for commits the `OpenPr` action pushes.
    #[arg(long, env = "GRADIENT_PR_COMMIT_NAME", default_value = "Gradient")]
    pub pr_commit_name: String,
    /// Author/committer email for commits the `OpenPr` action pushes.
    #[arg(long, env = "GRADIENT_PR_COMMIT_EMAIL", default_value = "gradient@localhost")]
    pub pr_commit_email: String,
    /// Who may create organizations through the API.
    #[arg(long, value_enum, env = "GRADIENT_CREATE_ORG", default_value_t = CreatePermission::Everyone)]
    pub create_org: CreatePermission,
    /// Who may create caches through the API.
    #[arg(long, value_enum, env = "GRADIENT_CREATE_CACHE", default_value_t = CreatePermission::Everyone)]
    pub create_cache: CreatePermission,
}

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            ip: "127.0.0.1".into(),
            port: 3000,
            serve_url: "http://127.0.0.1:8000".into(),
            frontend_url: "http://127.0.0.1:8000".into(),
            use_tls: true,
            pr_commit_name: "Gradient".into(),
            pr_commit_email: "gradient@localhost".into(),
            create_org: CreatePermission::default(),
            create_cache: CreatePermission::default(),
        }
    }
}
