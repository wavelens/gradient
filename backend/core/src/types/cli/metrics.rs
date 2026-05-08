/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct MetricsArgs {
    /// Path to a file containing the bearer token required to scrape
    /// `/metrics`. When unset, the metrics endpoint is disabled and
    /// returns 404. The file is read once at startup.
    #[arg(long, env = "GRADIENT_METRICS_TOKEN_FILE")]
    pub metrics_token_file: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_no_token_file() {
        let args = MetricsArgs::default();
        assert!(args.metrics_token_file.is_none());
    }
}
