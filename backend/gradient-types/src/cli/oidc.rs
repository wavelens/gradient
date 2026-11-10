/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("oidc")]
#[derive(Args, Debug, Clone)]
pub struct OidcArgs {
    /// Whether to enable OIDC.
    #[arg(id = "oidc-enable", default_value = "false")]
    pub enable: bool,
    /// Whether to enable OIDC as the only login method.
    #[arg(default_value = "false")]
    pub required: bool,
    /// OIDC client ID.
    #[arg(id = "oidc-client-id")]
    pub client_id: Option<String>,
    #[arg(id = "oidc-client-secret-file")]
    pub client_secret_file: Option<String>,
    /// OIDC scopes to request.
    #[arg(value_delimiter = ',', default_value = "openid,email,profile")]
    pub scopes: Vec<String>,
    /// OIDC discovery URL.
    pub discovery_url: Option<String>,
}
