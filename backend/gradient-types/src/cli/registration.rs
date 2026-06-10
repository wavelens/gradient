/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

pub const DEFAULT_SENTRY_DSN: &str =
    "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1";

#[derive(Args, Debug, Clone)]
pub struct RegistrationArgs {
    #[arg(long, env = "GRADIENT_ENABLE_REGISTRATION", default_value = "true")]
    pub enable_registration: bool,
    #[arg(long, env = "GRADIENT_REPORT_ERRORS", default_value = "false")]
    pub report_errors: bool,
    #[arg(long, env = "GRADIENT_SENTRY_DSN")]
    pub sentry_dsn: Option<String>,
}

impl Default for RegistrationArgs {
    fn default() -> Self {
        Self {
            enable_registration: true,
            report_errors: false,
            sentry_dsn: None,
        }
    }
}

pub fn effective_sentry_dsn(args: &RegistrationArgs) -> &str {
    args.sentry_dsn.as_deref().unwrap_or(DEFAULT_SENTRY_DSN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_sentry_dsn_returns_default_when_none() {
        let args = RegistrationArgs {
            enable_registration: true,
            report_errors: true,
            sentry_dsn: None,
        };
        assert_eq!(effective_sentry_dsn(&args), DEFAULT_SENTRY_DSN);
    }

    #[test]
    fn effective_sentry_dsn_returns_override_when_some() {
        let args = RegistrationArgs {
            enable_registration: true,
            report_errors: true,
            sentry_dsn: Some("https://example.invalid/9".to_string()),
        };
        assert_eq!(effective_sentry_dsn(&args), "https://example.invalid/9");
    }
}
