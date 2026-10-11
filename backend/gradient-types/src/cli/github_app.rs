/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU64;

use clap::Args;
use gradient_derive::config_group;

#[config_group("github_app")]
#[derive(Args, Debug, Clone)]
pub struct GitHubAppArgs {
    /// GitHub App ID, shown on the App's settings page.
    pub id: Option<NonZeroU64>,
    /// Path to the GitHub App RS256 private key PEM file.
    pub private_key_file: Option<String>,
    /// Path to a file containing the shared secret used to verify incoming
    /// GitHub App webhook payloads (`X-Hub-Signature-256`). The file's
    /// contents must match the value configured on the GitHub App's webhook
    /// settings page.
    pub webhook_secret_file: Option<String>,
}
