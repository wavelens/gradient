/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("scim")]
#[derive(Args, Debug, Clone)]
pub struct ScimArgs {
    /// Whether to enable SCIM provisioning.
    #[arg(id = "scim-enable", default_value = "false")]
    pub enable: bool,
    #[arg(id = "scim-token-file")]
    pub token_file: Option<String>,
    /// Whether to enable deleting users on SCIM `DELETE` instead of disabling them.
    #[arg(default_value = "false")]
    pub hard_delete: bool,
}
