/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskRefusal {
    #[error("input_name must not be empty")]
    EmptyInputName,
    #[error("input_name may contain letters, digits, _ - and glob chars * ? [ ]")]
    InvalidInputNameCharacters,
    #[error("input_name must match ^[A-Za-z_][A-Za-z0-9_-]*$")]
    InvalidInputNameStart,
    #[error("url must not be empty")]
    EmptyUrl,
    #[error("override for input '{input_name}' already exists")]
    OverrideExists { input_name: String },
    #[error("{0}")]
    InvalidTriggerConfig(String),
    #[error("trigger is inactive")]
    InactiveTrigger,
    #[error("Invalid display name: {reason}")]
    InvalidDisplayName { reason: String },
    #[error("{0}")]
    InvalidRepositoryUrl(String),
    #[error("{0}")]
    InvalidWildcard(String),
    #[error("keep_evaluations must be at least 1")]
    KeepEvaluationsBelowMinimum,
    #[error("keep_evaluations cannot exceed the server maximum of {maximum}")]
    KeepEvaluationsAboveMaximum { maximum: i32 },
    #[error("Failed to check repository: {reason}")]
    RepositoryCheckFailed { reason: String },
    #[error("Only the task owner or a project admin can transfer ownership.")]
    TransferNotPermitted,
    #[error("Cannot transfer ownership of a state-managed task.")]
    ManagedTaskTransfer,
    #[error("Task is already in this project.")]
    AlreadyInProject,
    #[error("team '{team}' has no access to this project")]
    TeamWithoutProjectAccess { team: String },
    #[error("{0}")]
    InvalidWebhookUrl(String),
    #[error("access_token must not be empty")]
    EmptyAccessToken,
    #[error("room_id must be a Matrix room ID like !abc123 or !abc:example.org")]
    InvalidMatrixRoomId,
    #[error("SMTP is not configured on this server")]
    SmtpNotConfigured,
    #[error("git_host_status_report actions cannot carry custom events")]
    StatusReportWithEvents,
    #[error("{action_type} requires {secret_field}")]
    MissingSecret {
        action_type: String,
        secret_field: String,
    },
    #[error("send_mail requires at least one recipient")]
    NoMailRecipients,
    #[error("integration is not an outbound integration")]
    IntegrationNotOutbound,
    #[error("outbound integration not found")]
    OutboundIntegrationNotFound,
    #[error("action with this name already exists")]
    ActionNameTaken,
    #[error("action_type cannot be changed")]
    ActionTypeChange,
    #[error("git_host_status_report and open_pr actions cannot carry custom events")]
    CustomEventsNotAllowed,
    #[error("regenerate-token is only valid for send_web_request actions")]
    TokenRegenerationNotSupported,
    #[error("`commit` must be a 40-character hex hash")]
    InvalidCommitHash,
    #[error("Invalid `attr`: {reason}")]
    InvalidAttr { reason: String },
    #[error("Commit not found in repository: {reason}")]
    CommitLookupFailed { reason: String },
    #[error("Failed to fetch repository state: {reason}")]
    RepositoryStateFetchFailed { reason: String },
    #[error("Evaluation already in progress")]
    EvaluationInProgress,
    #[error("`attr` must be a concrete attribute path, without wildcard syntax")]
    AttrNotConcrete,
    #[error("Unknown evaluation status `{name}`; expected `active`, `terminal`, or a status name")]
    UnknownEvaluationStatus { name: String },
    #[error("Invalid token")]
    InvalidToken,
    #[error("API key not allowed from this source IP")]
    SourceIpNotAllowed,
    #[error("Authorization required")]
    AuthorizationRequired,
}

impl Refusal for TaskRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::EmptyInputName
            | Self::InvalidInputNameCharacters
            | Self::InvalidInputNameStart
            | Self::EmptyUrl
            | Self::OverrideExists { .. }
            | Self::InvalidTriggerConfig(_)
            | Self::InactiveTrigger
            | Self::InvalidDisplayName { .. }
            | Self::InvalidRepositoryUrl(_)
            | Self::InvalidWildcard(_)
            | Self::KeepEvaluationsBelowMinimum
            | Self::KeepEvaluationsAboveMaximum { .. }
            | Self::RepositoryCheckFailed { .. }
            | Self::AlreadyInProject
            | Self::InvalidCommitHash
            | Self::InvalidAttr { .. }
            | Self::CommitLookupFailed { .. }
            | Self::RepositoryStateFetchFailed { .. }
            | Self::EvaluationInProgress
            | Self::AttrNotConcrete
            | Self::UnknownEvaluationStatus { .. } => RefusalStatus::BadRequest,
            Self::TransferNotPermitted | Self::ManagedTaskTransfer | Self::SourceIpNotAllowed => {
                RefusalStatus::Forbidden
            }
            Self::InvalidToken | Self::AuthorizationRequired => RefusalStatus::Unauthorized,
            Self::ActionNameTaken => RefusalStatus::Conflict,
            Self::TeamWithoutProjectAccess { .. }
            | Self::InvalidWebhookUrl(_)
            | Self::EmptyAccessToken
            | Self::InvalidMatrixRoomId
            | Self::SmtpNotConfigured
            | Self::StatusReportWithEvents
            | Self::MissingSecret { .. }
            | Self::NoMailRecipients
            | Self::IntegrationNotOutbound
            | Self::OutboundIntegrationNotFound
            | Self::ActionTypeChange
            | Self::CustomEventsNotAllowed
            | Self::TokenRegenerationNotSupported => RefusalStatus::UnprocessableEntity,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::RepositoryCheckFailed { .. }
            | Self::CommitLookupFailed { .. }
            | Self::RepositoryStateFetchFailed { .. } => ErrorCode::REPOSITORY_UNREACHABLE,
            Self::SourceIpNotAllowed => ErrorCode::FORBIDDEN_SOURCE_IP,
            Self::ActionNameTaken => ErrorCode::ALREADY_EXISTS,
            _ => self.status().default_code(),
        }
    }
}
