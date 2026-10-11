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
    pub client_id: Option<String>,
    pub client_secret_file: Option<String>,
    /// OIDC scopes to request.
    pub scopes: Option<String>,
    /// OIDC discovery URL.
    pub discovery_url: Option<String>,
}
