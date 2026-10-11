/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum TeamRefusal {
    #[error("A grant includes users, workers or both.")]
    EmptyGrant,
    #[error("A grant includes users, workers or both. Remove the grant instead.")]
    EmptyGrantAfterUpdate,
    #[error("You do not have permission to perform this action.")]
    GrantNotPermitted,
    #[error("A role is required when the grant includes users.")]
    RoleRequiredForUsers,
    #[error("Turning on a team's workers needs Admin in the team.")]
    WorkersNeedTeamAdmin,
    #[error(
        "Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API."
    )]
    ManagedProjectNotEditable,
    #[error(
        "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API."
    )]
    ManagedCacheNotEditable,
    #[error("No pending invitation for this user")]
    NoPendingInvitation,
    #[error("Invalid display name: {reason}")]
    InvalidDisplayName { reason: String },
    #[error("new_project_role must be Admin, Write or View")]
    InvalidNewProjectRole,
    #[error("member is managed by server state")]
    MemberManagedByState,
    #[error("Cannot remove the last Admin from the team.")]
    LastAdmin,
    #[error("User not in Team")]
    NotATeamMember,
    #[error("worker_id must be a valid UUID")]
    InvalidWorkerId,
    #[error("the worker id is already registered")]
    WorkerIdRegistered,
    #[error("worker is managed by server state")]
    WorkerManagedByState,
}

impl Refusal for TeamRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::EmptyGrant
            | Self::EmptyGrantAfterUpdate
            | Self::RoleRequiredForUsers
            | Self::NoPendingInvitation
            | Self::InvalidDisplayName { .. }
            | Self::InvalidNewProjectRole
            | Self::NotATeamMember
            | Self::InvalidWorkerId => RefusalStatus::BadRequest,
            Self::GrantNotPermitted
            | Self::WorkersNeedTeamAdmin
            | Self::ManagedProjectNotEditable
            | Self::ManagedCacheNotEditable => RefusalStatus::Forbidden,
            Self::MemberManagedByState
            | Self::LastAdmin
            | Self::WorkerIdRegistered
            | Self::WorkerManagedByState => RefusalStatus::Conflict,
        }
    }
}
