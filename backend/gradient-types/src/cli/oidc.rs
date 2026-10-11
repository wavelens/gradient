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
    #[arg(id = "oidc-enable", default_value = "false")]
    pub enable: bool,
    #[arg(default_value = "false")]
    pub required: bool,
    pub client_id: Option<String>,
    pub client_secret_file: Option<String>,
    pub scopes: Option<String>,
    pub discovery_url: Option<String>,
}
