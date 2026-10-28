/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorCode, WebError};
use crate::resource::Resource;
use std::fmt::{self, Display};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalStatus {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Gone,
    PayloadTooLarge,
    UnprocessableEntity,
    BadGateway,
    ServiceUnavailable,
}

impl RefusalStatus {
    pub(crate) const fn default_code(self) -> ErrorCode {
        match self {
            Self::BadRequest => ErrorCode::BAD_REQUEST,
            Self::Unauthorized => ErrorCode::UNAUTHORIZED,
            Self::Forbidden => ErrorCode::FORBIDDEN,
            Self::NotFound => ErrorCode::NOT_FOUND,
            Self::Conflict => ErrorCode::CONFLICT,
            Self::Gone => ErrorCode::GONE,
            Self::PayloadTooLarge => ErrorCode::PAYLOAD_TOO_LARGE,
            Self::UnprocessableEntity => ErrorCode::UNPROCESSABLE_ENTITY,
            Self::BadGateway => ErrorCode::BAD_GATEWAY,
            Self::ServiceUnavailable => ErrorCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// Implement this for the refusal enum of an endpoint area. Put an answer of several areas into
/// `SharedRefusal`, with the differing words as fields.
pub trait Refusal: Display {
    fn status(&self) -> RefusalStatus;

    fn code(&self) -> ErrorCode {
        self.status().default_code()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Modify,
    Delete,
    Revoke,
    TransferOwnership,
}

impl Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Modify => "modify",
            Self::Delete => "delete",
            Self::Revoke => "revoke",
            Self::TransferOwnership => "transfer ownership of",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SharedRefusal {
    #[error("Cannot {0} a state-managed {1}.")]
    StateManaged(Change, Resource),
    #[error("Cannot {0} the built-in roles (Admin, Write, View).")]
    BuiltinRole(Change),
    #[error("Role is still assigned to members. Reassign them before deleting the role.")]
    RoleAssignedToMembers,
    #[error("Role is still granted to a team. Change the team grant before deleting the role.")]
    RoleGrantedToTeam,
    #[error("User not in {0}")]
    NotAMember(Resource),
    #[error("No pending invitation for this user")]
    NoPendingInvitation,
    #[error("Cannot remove the last Admin from the {0}.")]
    LastAdmin(Resource),
    #[error("worker_id must be a valid UUID")]
    InvalidWorkerId,
    #[error("worker is managed by server state")]
    WorkerManagedByState,
    #[error("{0} must not be empty")]
    EmptyField(&'static str),
    #[error("API key not allowed from this source IP")]
    SourceIpNotAllowed,
    #[error("invalid allowed_ips entry '{entry}': {reason}")]
    InvalidAllowedIp { entry: String, reason: String },
    #[error("Invalid token")]
    InvalidToken,
    #[error("Authorization required")]
    AuthorizationRequired,
    #[error("account is deactivated")]
    AccountDeactivated,
    #[error("You do not have permission to perform this action.")]
    MissingPermission,
    #[error("Failed to fetch repository state: {reason}")]
    RepositoryStateFetchFailed { reason: String },
}

impl Refusal for SharedRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::StateManaged(..)
            | Self::BuiltinRole(_)
            | Self::SourceIpNotAllowed
            | Self::AccountDeactivated
            | Self::MissingPermission => RefusalStatus::Forbidden,
            Self::RoleAssignedToMembers
            | Self::RoleGrantedToTeam
            | Self::NotAMember(_)
            | Self::NoPendingInvitation
            | Self::InvalidWorkerId
            | Self::EmptyField(_)
            | Self::InvalidAllowedIp { .. }
            | Self::RepositoryStateFetchFailed { .. } => RefusalStatus::BadRequest,
            Self::LastAdmin(_) | Self::WorkerManagedByState => RefusalStatus::Conflict,
            Self::InvalidToken | Self::AuthorizationRequired => RefusalStatus::Unauthorized,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::SourceIpNotAllowed => ErrorCode::FORBIDDEN_SOURCE_IP,
            Self::InvalidAllowedIp { .. } => ErrorCode::INVALID_ALLOWED_IP,
            Self::RepositoryStateFetchFailed { .. } => ErrorCode::REPOSITORY_UNREACHABLE,
            _ => self.status().default_code(),
        }
    }
}

impl<R: Refusal> From<R> for WebError {
    fn from(refusal: R) -> Self {
        let code = refusal.code();
        let message = refusal.to_string();
        match refusal.status() {
            RefusalStatus::BadRequest => Self::BadRequest(code, message),
            RefusalStatus::Unauthorized => Self::Unauthorized(code, message),
            RefusalStatus::Forbidden => Self::Forbidden(code, message),
            RefusalStatus::NotFound => Self::NotFound(code, message),
            RefusalStatus::Conflict => Self::Conflict(code, message),
            RefusalStatus::Gone => Self::Gone(code, message),
            RefusalStatus::PayloadTooLarge => Self::PayloadTooLarge(code, message),
            RefusalStatus::UnprocessableEntity => Self::UnprocessableEntity(code, message),
            RefusalStatus::BadGateway => Self::BadGateway(code, message),
            RefusalStatus::ServiceUnavailable => Self::ServiceUnavailable(code, message),
        }
    }
}
