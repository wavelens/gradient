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
    #[error(
        "Create a connection of type GitHub with the ID of the installation of the GitHub App."
    )]
    InstallationIdRequired,
    #[error(
        "This server has no GitHub App. GitHub connections don't work. Ask the instance admin to set up a GitHub App."
    )]
    GitHubAppNotConfigured,
    #[error("GitHub cannot find the installation of this app: {reason}")]
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
        "Connections that receive events require a webhook secret. Enter a new webhook secret instead of clearing it."
    )]
    WebhookSecretRequired,
    #[error("Use an http or https URL as the address of the Git host.")]
    InvalidBaseUrl,
    #[error("Enable receiving events, reporting commit status, or both.")]
    NoCapability,
    #[error("The Git host URL and access token are required to report commit status.")]
    StatusNeedsBaseUrlAndToken,
    #[error(
        "This project can't use Git host connection {id}. Select a connection for the project, its owner team, or the instance."
    )]
    NotAvailableToProject { id: GitHostConnectionId },
    #[error("The repository does not match the connection {name}: {reason}")]
    RepositoryDoesNotMatch { name: String, reason: String },
    #[error(
        "Connection {name} doesn't report commit statuses. Turn on commit status reporting for this connection, or choose another connection."
    )]
    ConnectionCannotReportStatus { name: String },
    #[error("Select a connection to the git host before enabling commit status reporting.")]
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
