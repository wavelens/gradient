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
    /// Whether to enable sending email.
    #[arg(id = "email-enable", default_value = "false")]
    pub enable: bool,
    /// Whether to enable email verification for new accounts.
    #[arg(default_value = "false")]
    pub require_verification: bool,
    /// SMTP server host name.
    pub smtp_host: Option<String>,
    /// SMTP server port.
    #[arg(default_value = "587")]
    pub smtp_port: u16,
    /// SMTP user name.
    pub smtp_username: Option<String>,
    pub smtp_password_file: Option<String>,
    /// Whether to enable TLS for SMTP.
    #[arg(default_value = "true")]
    pub smtp_use_tls: bool,
    /// Sender email address.
    pub from_address: Option<String>,
    /// Sender display name.
    #[arg(default_value = "Gradient")]
    pub from_name: String,
}
