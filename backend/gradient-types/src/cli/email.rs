/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct EmailArgs {
    #[arg(
        id = "email-enable",
        long = "email-enable",
        env = "GRADIENT_EMAIL_ENABLE",
        default_value = "false"
    )]
    pub enable: bool,
    #[arg(
        long = "email-require-verification",
        env = "GRADIENT_EMAIL_REQUIRE_VERIFICATION",
        default_value = "false"
    )]
    pub require_verification: bool,
    #[arg(long = "email-smtp-host", env = "GRADIENT_EMAIL_SMTP_HOST")]
    pub smtp_host: Option<String>,
    #[arg(
        long = "email-smtp-port",
        env = "GRADIENT_EMAIL_SMTP_PORT",
        default_value = "587"
    )]
    pub smtp_port: u16,
    #[arg(long = "email-smtp-username", env = "GRADIENT_EMAIL_SMTP_USERNAME")]
    pub smtp_username: Option<String>,
    #[arg(
        long = "email-smtp-password-file",
        env = "GRADIENT_EMAIL_SMTP_PASSWORD_FILE"
    )]
    pub smtp_password_file: Option<String>,
    #[arg(
        long = "email-smtp-use-tls",
        env = "GRADIENT_EMAIL_SMTP_USE_TLS",
        default_value = "true"
    )]
    pub smtp_use_tls: bool,
    #[arg(long = "email-from-address", env = "GRADIENT_EMAIL_FROM_ADDRESS")]
    pub from_address: Option<String>,
    #[arg(
        long = "email-from-name",
        env = "GRADIENT_EMAIL_FROM_NAME",
        default_value = "Gradient"
    )]
    pub from_name: String,
}

impl Default for EmailArgs {
    fn default() -> Self {
        Self {
            enable: false,
            require_verification: false,
            smtp_host: None,
            smtp_port: 587,
            smtp_username: None,
            smtp_password_file: None,
            smtp_use_tls: true,
            from_address: None,
            from_name: "Gradient".into(),
        }
    }
}
