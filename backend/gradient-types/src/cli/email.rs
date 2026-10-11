/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("email")]
#[derive(Args, Debug, Clone)]
pub struct EmailArgs {
    #[arg(id = "email-enable", default_value = "false")]
    pub enable: bool,
    #[arg(default_value = "false")]
    pub require_verification: bool,
    pub smtp_host: Option<String>,
    #[arg(default_value = "587")]
    pub smtp_port: u16,
    pub smtp_username: Option<String>,
    pub smtp_password_file: Option<String>,
    #[arg(default_value = "true")]
    pub smtp_use_tls: bool,
    pub from_address: Option<String>,
    #[arg(default_value = "Gradient")]
    pub from_name: String,
}
