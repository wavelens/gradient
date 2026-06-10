/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct OidcArgs {
    #[arg(long, env = "GRADIENT_OIDC_ENABLED", default_value = "false")]
    pub oidc_enabled: bool,
    #[arg(long, env = "GRADIENT_OIDC_REQUIRED", default_value = "false")]
    pub oidc_required: bool,
    #[arg(long, env = "GRADIENT_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,
    #[arg(long, env = "GRADIENT_OIDC_CLIENT_SECRET_FILE")]
    pub oidc_client_secret_file: Option<String>,
    #[arg(long, env = "GRADIENT_OIDC_SCOPES")]
    pub oidc_scopes: Option<String>,
    #[arg(long, env = "GRADIENT_OIDC_DISCOVERY_URL")]
    pub oidc_discovery_url: Option<String>,
}
