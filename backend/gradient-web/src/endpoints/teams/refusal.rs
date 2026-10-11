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
    #[error("A role is required when the grant includes users.")]
    RoleRequiredForUsers,
    #[error("Turning on a team's workers needs Admin in the team.")]
    WorkersNeedTeamAdmin,
    #[error("new_project_role must be Admin, Write or View")]
    InvalidNewProjectRole,
    #[error("member is managed by server state")]
    MemberManagedByState,
    #[error("the worker id is already registered")]
    WorkerIdRegistered,
}

impl Refusal for TeamRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::EmptyGrant
            | Self::EmptyGrantAfterUpdate
            | Self::RoleRequiredForUsers
            | Self::InvalidNewProjectRole => RefusalStatus::BadRequest,
            Self::WorkersNeedTeamAdmin => RefusalStatus::Forbidden,
            Self::MemberManagedByState | Self::WorkerIdRegistered => RefusalStatus::Conflict,
        }
    }
}
