/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectRefusal {
    #[error("Invalid integration kind '{kind}': expected 'inbound' or 'outbound'.")]
    InvalidIntegrationKind { kind: String },
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
        "an integration named '{name}' already exists in this project; rename it before binding this installation"
    )]
    IntegrationNameTaken { name: String },
    #[error("GitHub App integrations are managed automatically and cannot be edited.")]
    GitHubIntegrationNotEditable,
    #[error(
        "GitHub integrations are managed through the server-wide GitHub App; enable the App on the project instead of switching git_host_type to github."
    )]
    SwitchToGitHubNotAllowed,
    #[error("Project not subscribed to Cache")]
    NotSubscribedToCache,
    #[error("the worker id belongs to a team worker")]
    WorkerIdBelongsToTeamWorker,
    #[error("Worker is already registered in this project")]
    WorkerIdRegistered,
    #[error("the worker id belongs to a team worker or a Gradient.CI connection")]
    WorkerIdBelongsToOtherConnection,
    #[error("team workers are managed on the team page")]
    TeamWorkerManagedOnTeamPage,
    #[error("{reason}")]
    InvalidSecretName { reason: String },
    #[error("secret value is empty")]
    EmptySecretValue,
    #[error("secret value is larger than 65536 bytes")]
    SecretValueTooLarge,
    #[error("secret \"{name}\" is managed by the declarative state")]
    ManagedSecret { name: String },
    #[error("project key unusable for sops: {reason}")]
    UnusableSopsKey { reason: String },
    #[error(
        "Inbound integrations need a webhook secret. Enter a new secret instead of clearing the current secret."
    )]
    WebhookSecretRequired,
    #[error("GitHub repositories connect through a GitHub App, not a token")]
    GitHubNeedsApp,
    #[error("endpoint_url must be an http or https URL")]
    InvalidEndpointUrl,
    #[error("No Gitea or Forgejo OAuth2 application configured on this server")]
    GiteaOAuth2ApplicationNotConfigured,
    #[error("Connections to the Git host need a signed-in browser session.")]
    GitHostOAuthConnectNeedsSignedInBrowserSession,
}

impl Refusal for ProjectRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::InvalidIntegrationKind { .. }
            | Self::InvalidGitHostType { .. }
            | Self::InstallationIdRequired
            | Self::GitHubAppNotConfigured
            | Self::InvalidInstallationId { .. }
            | Self::GitHubIntegrationNotEditable
            | Self::SwitchToGitHubNotAllowed
            | Self::NotSubscribedToCache
            | Self::InvalidSecretName { .. }
            | Self::EmptySecretValue
            | Self::SecretValueTooLarge
            | Self::WebhookSecretRequired
            | Self::GitHubNeedsApp
            | Self::InvalidEndpointUrl
            | Self::GiteaOAuth2ApplicationNotConfigured => RefusalStatus::BadRequest,
            Self::GitHostOAuthConnectNeedsSignedInBrowserSession => RefusalStatus::Unauthorized,
            Self::IntegrationNameTaken { .. }
            | Self::WorkerIdBelongsToTeamWorker
            | Self::WorkerIdRegistered
            | Self::WorkerIdBelongsToOtherConnection
            | Self::TeamWorkerManagedOnTeamPage
            | Self::ManagedSecret { .. }
            | Self::UnusableSopsKey { .. } => RefusalStatus::Conflict,
        }
    }
}
