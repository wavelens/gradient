/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitHostHookRefusal {
    #[error("No GitHub App configured")]
    GithubAppNotConfigured,
    #[error("invalid webhook signature")]
    InvalidSignature,
    #[error("malformed webhook payload")]
    MalformedPayload,
    #[error("No Git host connection at this address. No events reach that connection.")]
    GitHostConnectionNotFoundOrNotReceivingEvents,
    #[error(
        "No Webhook secret set for the connection to the Git host. Set a secret for the connection in Gradient."
    )]
    SecretNotConfigured,
    #[error("unsupported Git host")]
    UnsupportedGitHost,
    #[error("Webhook source IP not allowed")]
    SourceIpNotAllowed,
}

impl Refusal for GitHostHookRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::GithubAppNotConfigured => RefusalStatus::ServiceUnavailable,
            Self::InvalidSignature | Self::SecretNotConfigured => RefusalStatus::Unauthorized,
            Self::MalformedPayload | Self::UnsupportedGitHost => RefusalStatus::BadRequest,
            Self::GitHostConnectionNotFoundOrNotReceivingEvents => RefusalStatus::NotFound,
            Self::SourceIpNotAllowed => RefusalStatus::Forbidden,
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::SourceIpNotAllowed => ErrorCode::FORBIDDEN_SOURCE_IP,
            _ => self.status().default_code(),
        }
    }
}
