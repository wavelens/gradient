/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::ErrorCode;
use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitHostHookRefusal {
    #[error("github app integration not configured")]
    GithubAppNotConfigured,
    #[error("invalid webhook signature")]
    InvalidSignature,
    #[error("malformed webhook payload")]
    MalformedPayload,
    #[error("integration not found")]
    IntegrationNotFound,
    #[error("unsupported Git host")]
    UnsupportedGitHost,
    #[error("Webhook source IP not allowed")]
    SourceIpNotAllowed,
}

impl Refusal for GitHostHookRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::GithubAppNotConfigured => RefusalStatus::ServiceUnavailable,
            Self::InvalidSignature => RefusalStatus::Unauthorized,
            Self::MalformedPayload | Self::UnsupportedGitHost => RefusalStatus::BadRequest,
            Self::IntegrationNotFound => RefusalStatus::NotFound,
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
