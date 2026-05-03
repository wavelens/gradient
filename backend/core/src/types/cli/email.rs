/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct EmailArgs {
    #[arg(long, env = "GRADIENT_EMAIL_ENABLED", default_value = "false")]
    pub email_enabled: bool,
    #[arg(
        long,
        env = "GRADIENT_EMAIL_REQUIRE_VERIFICATION",
        default_value = "false"
    )]
    pub email_require_verification: bool,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_HOST")]
    pub email_smtp_host: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_PORT", default_value = "587")]
    pub email_smtp_port: u16,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_USERNAME")]
    pub email_smtp_username: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_PASSWORD_FILE")]
    pub email_smtp_password_file: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_FROM_ADDRESS")]
    pub email_from_address: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_FROM_NAME", default_value = "Gradient")]
    pub email_from_name: String,
    #[arg(long, env = "GRADIENT_EMAIL_ENABLE_TLS", default_value = "true")]
    pub email_enable_tls: bool,
}

impl Default for EmailArgs {
    fn default() -> Self {
        Self {
            email_enabled: false,
            email_require_verification: false,
            email_smtp_host: None,
            email_smtp_port: 587,
            email_smtp_username: None,
            email_smtp_password_file: None,
            email_from_address: None,
            email_from_name: "Gradient".into(),
            email_enable_tls: true,
        }
    }
}
