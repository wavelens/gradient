/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorCode, WebError};
use std::fmt::Display;

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
            Self::ServiceUnavailable => ErrorCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// Implement this for the refusal enum of an endpoint area. Add a variant for every new answer
/// to a refused request; `?` and `.into()` turn it into a `WebError`.
pub trait Refusal: Display {
    fn status(&self) -> RefusalStatus;

    fn code(&self) -> ErrorCode {
        self.status().default_code()
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
            RefusalStatus::ServiceUnavailable => Self::ServiceUnavailable(code, message),
        }
    }
}
