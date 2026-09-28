/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

pub const DEFAULT_SENTRY_DSN: &str =
    "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1";

#[derive(Args, Debug, Clone, Default)]
pub struct SentryArgs {
    #[arg(
        id = "sentry-enable",
        long = "sentry-enable",
        env = "GRADIENT_SENTRY_ENABLE",
        default_value = "false"
    )]
    pub enable: bool,
    #[arg(long = "sentry-dsn", env = "GRADIENT_SENTRY_DSN")]
    pub dsn: Option<String>,
}

pub fn effective_sentry_dsn(args: &SentryArgs) -> &str {
    args.dsn.as_deref().unwrap_or(DEFAULT_SENTRY_DSN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_sentry_dsn_returns_default_when_none() {
        let args = SentryArgs {
            enable: true,
            dsn: None,
        };
        assert_eq!(effective_sentry_dsn(&args), DEFAULT_SENTRY_DSN);
    }

    #[test]
    fn effective_sentry_dsn_returns_override_when_some() {
        let args = SentryArgs {
            enable: true,
            dsn: Some("https://example.invalid/9".to_string()),
        };
        assert_eq!(effective_sentry_dsn(&args), "https://example.invalid/9");
    }
}
