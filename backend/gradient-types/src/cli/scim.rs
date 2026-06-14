/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct ScimArgs {
    #[arg(long, env = "GRADIENT_SCIM_ENABLED", default_value = "false")]
    pub scim_enabled: bool,
    #[arg(long, env = "GRADIENT_SCIM_TOKEN_FILE")]
    pub scim_token_file: Option<String>,
    #[arg(long, env = "GRADIENT_SCIM_HARD_DELETE", default_value = "false")]
    pub scim_hard_delete: bool,
}
