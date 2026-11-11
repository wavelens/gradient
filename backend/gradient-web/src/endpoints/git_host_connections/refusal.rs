/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};
use gradient_types::GitHostConnectionId;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitHostConnectionRefusal {
    #[error(
        "Invalid Git host type '{host_type}': expected 'gitea', 'forgejo', 'gitlab', or 'github'."
    )]
    InvalidGitHostType { host_type: String },
    #[error("git_host_type 'github' requires an installation_id")]
    InstallationIdRequired,
    #[error(
        "No GitHub App on this server. GitHub integration isn't available. Ask an instance admin to set up the GitHub App."
    )]
    GitHubAppNotConfigured,
    #[error("invalid installation_id: {reason}")]
    InvalidInstallationId { reason: String },
    #[error(
        "Installations manage GitHub App connections. No edit options exist for this connection."
    )]
    GitHubConnectionNotEditable,
    #[error(
        "The connection type can't change to github. GitHub repositories connect through the GitHub App of this server."
    )]
    SwitchToGitHubNotAllowed,
    #[error(
        "GitHub repositories connect through the GitHub App on this server. Ask an instance admin to install the App. All projects see the installation as a connection."
    )]
    GitHubNeedsInstanceApp,
    #[error(
        "Connections with `receives_events` need the webhook secret. Don't delete the webhook secret. Enter a new webhook secret instead."
    )]
    WebhookSecretRequired,
    #[error("Use an http or https URL for base_url.")]
    InvalidBaseUrl,
    #[error("Enable `receives_events`, `reports_status`, or both.")]
    NoCapability,
    #[error("Commit status reports need `base_url` and `access_token`.")]
    StatusNeedsBaseUrlAndToken,
    #[error(
        "This project can't use Git host connection {id}. Select a connection for the project, its owner team, or the instance."
    )]
    NotAvailableToProject { id: GitHostConnectionId },
    #[error("The repository does not match the connection {name}: {reason}")]
    RepositoryDoesNotMatch { name: String, reason: String },
    #[error(
        "Connection {name} does not report commit status. Enable reports_status on the connection, or choose another connection."
    )]
    ConnectionCannotReportStatus { name: String },
    #[error("Connect to a Git host before report_status.")]
    ReportStatusNeedsConnection,
    #[error("{0}")]
    StatusTestNotPossible(String),
    #[error("No Gitea or Forgejo OAuth2 application configured on this server")]
    GiteaOAuth2ApplicationNotConfigured,
    #[error("Connections to the Git host need a signed-in browser session.")]
    GitHostOAuthConnectNeedsSignedInBrowserSession,
}

impl Refusal for GitHostConnectionRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::StatusTestNotPossible(_) => RefusalStatus::Conflict,
            Self::GitHostOAuthConnectNeedsSignedInBrowserSession => RefusalStatus::Unauthorized,
            _ => RefusalStatus::BadRequest,
        }
    }
}
