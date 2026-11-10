/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskRefusal {
    #[error("input_name may contain letters, digits, _ - and glob chars * ? [ ]")]
    InvalidInputNameCharacters,
    #[error("input_name must match ^[A-Za-z_][A-Za-z0-9_-]*$")]
    InvalidInputNameStart,
    #[error("override for input '{input_name}' already exists")]
    OverrideExists { input_name: String },
    #[error("{0}")]
    InvalidTriggerConfig(String),
    #[error("trigger is inactive")]
    InactiveTrigger,
    #[error("the default trigger cannot be deleted, make another trigger the default first")]
    DefaultTriggerDeletion,
    #[error("{0}")]
    InvalidRepositoryUrl(String),
    #[error("{0}")]
    InvalidWildcard(String),
    #[error("Keep Evaluations must be at least 1")]
    KeepEvaluationsBelowMinimum,
    #[error("Keep Evaluations cannot exceed the server maximum {maximum}")]
    KeepEvaluationsAboveMaximum { maximum: i32 },
    #[error("Failed to access the repository: {reason}")]
    RepositoryCheckFailed { reason: String },
    #[error("Only the task owner or a project admin can transfer ownership.")]
    TransferNotPermitted,
    #[error("Task is already in this project.")]
    AlreadyInProject,
    #[error(
        "The triggers or actions of the task use the integration with the Git host for this project. Delete them first, then move the task."
    )]
    CurrentProjectGitHostIntegrationInUse,
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
    #[error("Select at least one event. The action does not run without an event.")]
    ActionHasNoEvents,
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
    #[error(
        "The repository has no such commit. Check the commit hash and confirm that Gradient can read the repository."
    )]
    CommitLookupFailed,
    #[error("Evaluation already in progress")]
    EvaluationInProgress,
    #[error("`attr` must be a concrete attribute path, without wildcard syntax")]
    AttrNotConcrete,
    #[error("Unknown evaluation status `{name}`; expected `active`, `terminal`, or a status name")]
    UnknownEvaluationStatus { name: String },
    #[error("the task to start belongs to another project")]
    StartTaskInOtherProject,
    #[error("this integration not part of this project, or wrong direction")]
    RepositoryIntegrationMismatch,
    #[error("Push and pull request triggers need a webhook receiving integration for this project")]
    TriggerIntegrationNotInbound,
    #[error(
        "no access token on the status reporter, add one first with admin rights on the repository"
    )]
    ProvisionTokenMissing,
    #[error(
        "the integration for the webhook receiver has no secret - set the secret before webhook installation"
    )]
    ProvisionSecretMissing,
    #[error("{0}")]
    ProvisionUnsupported(String),
    #[error("git host did not accept the request: {reason}")]
    ProvisionRefused { reason: String },
}

impl Refusal for TaskRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::InvalidInputNameCharacters
            | Self::InvalidInputNameStart
            | Self::OverrideExists { .. }
            | Self::InvalidTriggerConfig(_)
            | Self::InactiveTrigger
            | Self::InvalidRepositoryUrl(_)
            | Self::InvalidWildcard(_)
            | Self::KeepEvaluationsBelowMinimum
            | Self::KeepEvaluationsAboveMaximum { .. }
            | Self::RepositoryCheckFailed { .. }
            | Self::AlreadyInProject
            | Self::InvalidCommitHash
            | Self::StartTaskInOtherProject
            | Self::InvalidAttr { .. }
            | Self::CommitLookupFailed
            | Self::EvaluationInProgress
            | Self::AttrNotConcrete
            | Self::UnknownEvaluationStatus { .. }
            | Self::RepositoryIntegrationMismatch
            | Self::TriggerIntegrationNotInbound => RefusalStatus::BadRequest,
            Self::TransferNotPermitted => RefusalStatus::Forbidden,
            Self::ActionNameTaken
            | Self::DefaultTriggerDeletion
            | Self::CurrentProjectGitHostIntegrationInUse => RefusalStatus::Conflict,
            Self::TeamWithoutProjectAccess { .. }
            | Self::InvalidWebhookUrl(_)
            | Self::EmptyAccessToken
            | Self::InvalidMatrixRoomId
            | Self::SmtpNotConfigured
            | Self::ActionHasNoEvents
            | Self::MissingSecret { .. }
            | Self::NoMailRecipients
            | Self::IntegrationNotOutbound
            | Self::OutboundIntegrationNotFound
            | Self::ActionTypeChange
            | Self::CustomEventsNotAllowed
            | Self::TokenRegenerationNotSupported
            | Self::ProvisionTokenMissing
            | Self::ProvisionSecretMissing
            | Self::ProvisionUnsupported(_) => RefusalStatus::UnprocessableEntity,
            Self::ProvisionRefused { .. } => RefusalStatus::BadGateway,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::RepositoryCheckFailed { .. } | Self::CommitLookupFailed => {
                ErrorCode::REPOSITORY_UNREACHABLE
            }
            Self::ActionNameTaken => ErrorCode::ALREADY_EXISTS,
            _ => self.status().default_code(),
        }
    }
}
