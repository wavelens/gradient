/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct ScimArgs {
    #[arg(
        id = "scim-enable",
        long = "scim-enable",
        env = "GRADIENT_SCIM_ENABLE",
        default_value = "false"
    )]
    pub enable: bool,
    #[arg(
        id = "scim-token-file",
        long = "scim-token-file",
        env = "GRADIENT_SCIM_TOKEN_FILE"
    )]
    pub token_file: Option<String>,
    #[arg(
        long = "scim-hard-delete",
        env = "GRADIENT_SCIM_HARD_DELETE",
        default_value = "false"
    )]
    pub hard_delete: bool,
}
