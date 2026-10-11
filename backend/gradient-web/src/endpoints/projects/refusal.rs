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
    #[error("GitHub App is not configured on this server; set GRADIENT_GITHUB_APP_* first")]
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
    #[error("the worker id belongs to a team worker or a Gradient.CI connection")]
    WorkerIdBelongsToOtherConnection,
    #[error("team workers are managed on the team page")]
    TeamWorkerManagedOnTeamPage,
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
            | Self::NotSubscribedToCache => RefusalStatus::BadRequest,
            Self::IntegrationNameTaken { .. }
            | Self::WorkerIdBelongsToTeamWorker
            | Self::WorkerIdBelongsToOtherConnection
            | Self::TeamWorkerManagedOnTeamPage => RefusalStatus::Conflict,
        }
    }
}
