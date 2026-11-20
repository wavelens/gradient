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
    #[error("A GitHub connection needs the ID of a GitHub App installation.")]
    InstallationIdRequired,
    #[error(
        "No GitHub App on this server. GitHub connections aren't available. Ask an instance admin to set up the GitHub App."
    )]
    GitHubAppNotConfigured,
    #[error("GitHub doesn't know this App installation: {reason}")]
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
        "A connection that receives events needs a webhook secret. Enter a new webhook secret instead of removing it."
    )]
    WebhookSecretRequired,
    #[error("Use an http or https URL for the Git host URL.")]
    InvalidBaseUrl,
    #[error("Turn on receiving events, reporting commit status, or both.")]
    NoCapability,
    #[error("Reporting commit status needs the Git host URL and an access token.")]
    StatusNeedsBaseUrlAndToken,
    #[error(
        "This project can't use Git host connection {id}. Select a connection for the project, its owner team, or the instance."
    )]
    NotAvailableToProject { id: GitHostConnectionId },
    #[error("The repository does not match the connection {name}: {reason}")]
    RepositoryDoesNotMatch { name: String, reason: String },
    #[error(
        "Connection {name} does not report commit status. Turn on reporting commit status for the connection, or choose another connection."
    )]
    ConnectionCannotReportStatus { name: String },
    #[error("Select a connection to the Git host before turning on commit status reports.")]
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
