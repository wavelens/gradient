/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectRefusal {
    #[error("invalid allowed_ips entry '{entry}': {reason}")]
    InvalidAllowedIp { entry: String, reason: String },
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
    #[error("display_name cannot be empty")]
    EmptyDisplayName,
    #[error(
        "GitHub integrations are managed through the server-wide GitHub App; enable the App on the project instead of switching git_host_type to github."
    )]
    SwitchToGitHubNotAllowed,
    #[error("No pending invitation for this user")]
    NoPendingInvitation,
    #[error("Invalid display name: {reason}")]
    InvalidDisplayName { reason: String },
    #[error("User not in Project")]
    NotAProjectMember,
    #[error("Built-in roles (Admin, Write, View) cannot be modified.")]
    BuiltinRoleNotEditable,
    #[error("State-managed roles cannot be modified via the API.")]
    ManagedRoleNotEditable,
    #[error("Built-in roles (Admin, Write, View) cannot be deleted.")]
    BuiltinRoleNotDeletable,
    #[error("State-managed roles cannot be deleted via the API.")]
    ManagedRoleNotDeletable,
    #[error("Role is still assigned to members. Reassign them before deleting the role.")]
    RoleAssignedToMembers,
    #[error("Role is still granted to a team. Change the team grant before deleting the role.")]
    RoleGrantedToTeam,
    #[error("Project not subscribed to Cache")]
    NotSubscribedToCache,
    #[error("worker_id must be a valid UUID")]
    InvalidWorkerId,
    #[error("the worker id belongs to a team worker")]
    WorkerIdBelongsToTeamWorker,
    #[error("the worker id belongs to a team worker or a Gradient.CI connection")]
    WorkerIdBelongsToOtherConnection,
    #[error("team workers are managed on the team page")]
    TeamWorkerManagedOnTeamPage,
    #[error("worker is managed by server state")]
    WorkerManagedByState,
}

impl Refusal for ProjectRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::InvalidAllowedIp { .. }
            | Self::InvalidIntegrationKind { .. }
            | Self::InvalidGitHostType { .. }
            | Self::InstallationIdRequired
            | Self::GitHubAppNotConfigured
            | Self::InvalidInstallationId { .. }
            | Self::GitHubIntegrationNotEditable
            | Self::EmptyDisplayName
            | Self::SwitchToGitHubNotAllowed
            | Self::NoPendingInvitation
            | Self::InvalidDisplayName { .. }
            | Self::NotAProjectMember
            | Self::RoleAssignedToMembers
            | Self::RoleGrantedToTeam
            | Self::NotSubscribedToCache
            | Self::InvalidWorkerId => RefusalStatus::BadRequest,
            Self::BuiltinRoleNotEditable
            | Self::ManagedRoleNotEditable
            | Self::BuiltinRoleNotDeletable
            | Self::ManagedRoleNotDeletable => RefusalStatus::Forbidden,
            Self::IntegrationNameTaken { .. }
            | Self::WorkerIdBelongsToTeamWorker
            | Self::WorkerIdBelongsToOtherConnection
            | Self::TeamWorkerManagedOnTeamPage
            | Self::WorkerManagedByState => RefusalStatus::Conflict,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidAllowedIp { .. } => ErrorCode::INVALID_ALLOWED_IP,
            _ => self.status().default_code(),
        }
    }
}
