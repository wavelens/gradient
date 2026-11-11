/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::permissions::Permission;
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
        "The task uses a connection to a Git host that the destination project can't use. Select another connection in the settings of the task repository or in the actions that open pull requests, then move the task."
    )]
    TaskUsesGitHostConnectionTargetProjectCannotUse,
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
    #[error("action with this name already exists")]
    ActionNameTaken,
    #[error("action_type cannot be changed")]
    ActionTypeChange,
    #[error("open_pr actions cannot carry custom events")]
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
    #[error("Select a Git host connection first on the repository page for the task.")]
    GitHostConnectionMissing,
    #[error(
        "The connection to the Git host for this task has no access token. Add a token with admin rights on the repository to the connection."
    )]
    ProvisionTokenMissing,
    #[error(
        "The task's connection to the Git host doesn't have a webhook secret or receive events. Turn on receives_events for the connection first."
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
            | Self::GitHostConnectionMissing => RefusalStatus::BadRequest,
            Self::TransferNotPermitted => RefusalStatus::Forbidden,
            Self::ActionNameTaken
            | Self::DefaultTriggerDeletion
            | Self::TaskUsesGitHostConnectionTargetProjectCannotUse => RefusalStatus::Conflict,
            Self::TeamWithoutProjectAccess { .. }
            | Self::InvalidWebhookUrl(_)
            | Self::EmptyAccessToken
            | Self::InvalidMatrixRoomId
            | Self::SmtpNotConfigured
            | Self::ActionHasNoEvents
            | Self::MissingSecret { .. }
            | Self::NoMailRecipients
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

    fn missing_permission_wire_names(&self) -> Vec<&'static str> {
        match self {
            Self::TransferNotPermitted => vec![Permission::EditTask.as_wire_name()],
            _ => Vec::new(),
        }
    }
}
