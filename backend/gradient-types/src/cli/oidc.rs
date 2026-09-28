/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct OidcArgs {
    #[arg(
        id = "oidc-enable",
        long = "oidc-enable",
        env = "GRADIENT_OIDC_ENABLE",
        default_value = "false"
    )]
    pub enable: bool,
    #[arg(
        long = "oidc-required",
        env = "GRADIENT_OIDC_REQUIRED",
        default_value = "false"
    )]
    pub required: bool,
    #[arg(long = "oidc-client-id", env = "GRADIENT_OIDC_CLIENT_ID")]
    pub client_id: Option<String>,
    #[arg(
        long = "oidc-client-secret-file",
        env = "GRADIENT_OIDC_CLIENT_SECRET_FILE"
    )]
    pub client_secret_file: Option<String>,
    #[arg(long = "oidc-scopes", env = "GRADIENT_OIDC_SCOPES")]
    pub scopes: Option<String>,
    #[arg(long = "oidc-discovery-url", env = "GRADIENT_OIDC_DISCOVERY_URL")]
    pub discovery_url: Option<String>,
}
