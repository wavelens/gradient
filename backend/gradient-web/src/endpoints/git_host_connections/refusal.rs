/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};
use gradient_db::git_host_connection_repository_grants::GitHostConnectionRepositoryGrantError;
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
        "GitHub repositories connect through the GitHub App on this server. Ask an instance admin to install the App. All projects see the installation as a connection. Tasks only use the installation with a repository grant that instance admins create for that project and that repository."
    )]
    GitHubNeedsInstanceApp,
    #[error(
        "Connections that receive events require a webhook secret. Enter a new webhook secret instead of clearing it."
    )]
    WebhookSecretRequired,
    #[error("Use an http or https URL as the address of the Git host. Reason: {0}")]
    InvalidBaseUrl(String),
    #[error(
        "Connections to Gitea or Forgejo need `base_url`, the http or https address of the Git host."
    )]
    GitHostConnectionBaseUrlMissing,
    #[error("Enable receiving events, reporting commit status, or both.")]
    NoCapability,
    #[error("The Git host URL and access token are required to report commit status.")]
    StatusNeedsBaseUrlAndToken,
    #[error("This task can't use Git host connection {id}: {refusal}")]
    GitHostConnectionCannotActOnTaskRepository {
        id: GitHostConnectionId,
        refusal: GitHostConnectionRepositoryGrantError,
    },
    #[error(
        "Connection {name} doesn't report commit statuses. Turn on commit status reporting for this connection, or choose another connection."
    )]
    ConnectionCannotReportStatus { name: String },
    #[error("Select a connection to a Git host before enabling commit status reporting.")]
    ReportStatusNeedsConnection,
    #[error("{0}")]
    StatusTestNotPossible(String),
    #[error("No Gitea or Forgejo OAuth2 application configured on this server")]
    GiteaOAuth2ApplicationNotConfigured,
    #[error("Connections to the Git host need a signed-in browser session.")]
    GitHostOAuthConnectNeedsSignedInBrowserSession,
    #[error(
        "The connection to this Git host either did not start in this browser session for this project, or it has expired. Start the connection again."
    )]
    GitHostOAuthConnectCookieMissingExpiredOrStartedByOtherSessionUserOrProject,
    #[error(
        "The Git host rejected the authorization code. Restart the connection from the beginning."
    )]
    GitHostRefusedOAuthAuthorizationCode,
    #[error(
        "{field} '{value}' is not a single path segment. Use only letters, digits, '.', '_' and '-'. Don't use '.' or '..'."
    )]
    RepositoryGrantOwnerNotSingleUrlPathSegment { field: &'static str, value: String },
    #[error(
        "repository_name '{value}' is not a repository path. Combine path segments of letters, digits, '.', '_' and '-' with '/', exclude empty segments and exclude '.' or '..'."
    )]
    RepositoryGrantRepositoryNameNotValidRepositoryPath { value: String },
    #[error(
        "repository_name '{value}' ends with '.git'. Leave out '.git', because Gradient strips that suffix from the repository URL of a task."
    )]
    RepositoryGrantNameEndsInGit { value: String },
    #[error("{field} is longer than {max_length} characters.")]
    RepositoryGrantOwnerOrRepositoryNameExceedsMaximumLength {
        field: &'static str,
        max_length: usize,
    },
    #[error(
        "Project \"{project}\" does not belong to this team. Only projects of this team can get a team connection."
    )]
    RepositoryGrantProjectNotInTeam { project: String },
}

impl Refusal for GitHostConnectionRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::StatusTestNotPossible(_) => RefusalStatus::Conflict,
            Self::GitHostConnectionCannotActOnTaskRepository {
                refusal: GitHostConnectionRepositoryGrantError::RepositoryGrantMissingForTaskProject,
                ..
            } => RefusalStatus::Forbidden,
            Self::GitHostOAuthConnectNeedsSignedInBrowserSession => RefusalStatus::Unauthorized,
            Self::GitHostConnectionBaseUrlMissing => RefusalStatus::UnprocessableEntity,
            Self::GitHostRefusedOAuthAuthorizationCode => RefusalStatus::BadGateway,
            _ => RefusalStatus::BadRequest,
        }
    }
}
