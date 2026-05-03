/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct GitHubAppArgs {
    /// GitHub App ID. Required to enable GitHub App webhook and CI reporting.
    #[arg(long, env = "GRADIENT_GITHUB_APP_ID")]
    pub github_app_id: Option<u64>,
    /// Path to the GitHub App RS256 private key PEM file.
    #[arg(long, env = "GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE")]
    pub github_app_private_key_file: Option<String>,
    /// Path to a file containing the shared secret used to verify incoming
    /// GitHub App webhook payloads (`X-Hub-Signature-256`). The file's
    /// contents must match the value configured on the GitHub App's webhook
    /// settings page.
    #[arg(long, env = "GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE")]
    pub github_app_webhook_secret_file: Option<String>,
}
