/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Web-layer error type.
//!
//! `WebError` collapses every failure mode the HTTP layer can produce into one
//! variant per HTTP status, plus a single `Internal` variant for chained
//! source errors. Each non-internal variant carries a stable [`ErrorCode`]
//! slug that is emitted in the JSON response body so clients can
//! programmatically branch on failures without parsing English prose.
//!
//! Construct errors through the `bad_request`, `not_found`, … helper
//! constructors (which set sensible default codes) or pass an explicit
//! [`ErrorCode`] for finer-grained semantics.

use anyhow::Error as AnyhowError;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sea_orm::{DbErr, RuntimeErr, sqlx};
use serde::Serialize;
use std::fmt;
use thiserror::Error;

/// Stable, machine-readable error slug returned in the JSON body alongside
/// the HTTP status. Codes are intentionally `&'static str` so they cost
/// nothing and can be exhaustively listed in API docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ErrorCode(pub &'static str);

impl ErrorCode {
    // 400 Bad Request
    pub const BAD_REQUEST: Self = Self("bad_request");
    pub const VALIDATION: Self = Self("validation");
    pub const INPUT_VALIDATION: Self = Self("input_validation");
    pub const JSON_PARSING: Self = Self("json_parsing");
    pub const INVALID_NAME: Self = Self("invalid_name");
    pub const INVALID_EMAIL: Self = Self("invalid_email");
    pub const INVALID_PASSWORD: Self = Self("invalid_password");
    pub const INVALID_USERNAME: Self = Self("invalid_username");
    pub const INVALID_OAUTH_CODE: Self = Self("invalid_oauth_code");
    pub const OAUTH_DISABLED: Self = Self("oauth_disabled");
    pub const REGISTRATION_DISABLED: Self = Self("registration_disabled");

    // 401 Unauthorized
    pub const UNAUTHORIZED: Self = Self("unauthorized");
    pub const AUTHENTICATION: Self = Self("authentication");
    pub const INVALID_CREDENTIALS: Self = Self("invalid_credentials");
    pub const OAUTH_REQUIRED: Self = Self("oauth_required");

    // 403 Forbidden
    pub const FORBIDDEN: Self = Self("forbidden");
    pub const SUPERUSER_REQUIRED: Self = Self("superuser_required");

    // 404 Not Found
    pub const NOT_FOUND: Self = Self("not_found");

    // 409 Conflict
    pub const CONFLICT: Self = Self("conflict");
    pub const ALREADY_EXISTS: Self = Self("already_exists");

    // 410 Gone
    pub const GONE: Self = Self("gone");

    // 413 Payload Too Large
    pub const PAYLOAD_TOO_LARGE: Self = Self("payload_too_large");

    // 422 Unprocessable Entity
    pub const UNPROCESSABLE_ENTITY: Self = Self("unprocessable_entity");

    // 500 Internal Server Error
    pub const INTERNAL: Self = Self("internal");
    pub const DATABASE: Self = Self("database");
    pub const DATA_INCONSISTENCY: Self = Self("data_inconsistency");
    pub const TOKEN_GENERATION_FAILED: Self = Self("token_generation_failed");
    pub const USER_UPDATE_FAILED: Self = Self("user_update_failed");
    pub const SSH_KEY_GENERATION_FAILED: Self = Self("ssh_key_generation_failed");

    // 503 Service Unavailable
    pub const SERVICE_UNAVAILABLE: Self = Self("service_unavailable");

    #[inline]
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Debug, Error)]
pub enum WebError {
    #[error("Bad Request [{0}]: {1}")]
    BadRequest(ErrorCode, String),
    #[error("Unauthorized [{0}]: {1}")]
    Unauthorized(ErrorCode, String),
    #[error("Forbidden [{0}]: {1}")]
    Forbidden(ErrorCode, String),
    #[error("Not Found [{0}]: {1}")]
    NotFound(ErrorCode, String),
    #[error("Conflict [{0}]: {1}")]
    Conflict(ErrorCode, String),
    #[error("Gone [{0}]: {1}")]
    Gone(ErrorCode, String),
    #[error("Payload Too Large [{0}]: {1}")]
    PayloadTooLarge(ErrorCode, String),
    #[error("Unprocessable Entity [{0}]: {1}")]
    UnprocessableEntity(ErrorCode, String),
    #[error("Service Unavailable [{0}]: {1}")]
    ServiceUnavailable(ErrorCode, String),
    /// Referential-integrity mismatch detected at request time (e.g. a build
    /// row whose derivation row was concurrently deleted). Maps to the same
    /// HTTP response as [`Internal`] but is logged at warn level — the
    /// rich-context warn line is emitted at the callsite, so `IntoResponse`
    /// stays silent.
    #[error("Data Inconsistency: {0}")]
    DataInconsistency(String),
    #[error(transparent)]
    Internal(#[from] AnyhowError),
}

pub type WebResult<T> = Result<T, WebError>;

/// JSON response body emitted for every `WebError`. Adds a stable `code`
/// alongside the existing `error`/`message` fields so clients can branch
/// without parsing prose.
#[derive(Serialize)]
struct ErrorResponseBody<'a> {
    error: bool,
    code: ErrorCode,
    message: &'a str,
}

impl WebError {
    /// HTTP status this error maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(..) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(..) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(..) => StatusCode::FORBIDDEN,
            Self::NotFound(..) => StatusCode::NOT_FOUND,
            Self::Conflict(..) => StatusCode::CONFLICT,
            Self::Gone(..) => StatusCode::GONE,
            Self::PayloadTooLarge(..) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnprocessableEntity(..) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::ServiceUnavailable(..) => StatusCode::SERVICE_UNAVAILABLE,
            Self::DataInconsistency(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable error slug returned in the JSON body.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::BadRequest(c, _)
            | Self::Unauthorized(c, _)
            | Self::Forbidden(c, _)
            | Self::NotFound(c, _)
            | Self::Conflict(c, _)
            | Self::Gone(c, _)
            | Self::PayloadTooLarge(c, _)
            | Self::UnprocessableEntity(c, _)
            | Self::ServiceUnavailable(c, _) => *c,
            Self::DataInconsistency(_) | Self::Internal(_) => ErrorCode::INTERNAL,
        }
    }
}

// ── Conversions ─────────────────────────────────────────────────────────

impl From<DbErr> for WebError {
    fn from(err: DbErr) -> Self {
        Self::Internal(AnyhowError::new(err))
    }
}

fn is_unique_violation(err: &DbErr) -> bool {
    let sqlx_err = match err {
        DbErr::Query(RuntimeErr::SqlxError(e)) | DbErr::Exec(RuntimeErr::SqlxError(e)) => e,
        _ => return false,
    };
    matches!(
        sqlx_err,
        sqlx::Error::Database(db_err) if db_err.is_unique_violation()
    )
}

impl From<gradient_core::types::input::InputError> for WebError {
    fn from(err: gradient_core::types::input::InputError) -> Self {
        Self::BadRequest(ErrorCode::INPUT_VALIDATION, err.to_string())
    }
}

impl From<JsonRejection> for WebError {
    fn from(err: JsonRejection) -> Self {
        Self::BadRequest(ErrorCode::JSON_PARSING, format!("Invalid JSON: {}", err))
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code();

        let message: String = match &self {
            Self::Internal(err) => {
                tracing::error!(error = format!("{err:#}"), "Internal error");
                "Internal server error".to_string()
            }
            Self::DataInconsistency(_) => {
                // Rich-context warn line is emitted at the construction
                // callsite — don't double-log here.
                "Internal server error".to_string()
            }
            Self::BadRequest(_, m)
            | Self::Unauthorized(_, m)
            | Self::Forbidden(_, m)
            | Self::NotFound(_, m)
            | Self::Conflict(_, m)
            | Self::Gone(_, m)
            | Self::PayloadTooLarge(_, m)
            | Self::UnprocessableEntity(_, m)
            | Self::ServiceUnavailable(_, m) => m.clone(),
        };

        let body = Json(ErrorResponseBody {
            error: true,
            code,
            message: &message,
        });

        (status, body).into_response()
    }
}

// ── Constructors ────────────────────────────────────────────────────────

impl WebError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(ErrorCode::BAD_REQUEST, msg.into())
    }

    pub fn bad_request_with(code: ErrorCode, msg: impl Into<String>) -> Self {
        Self::BadRequest(code, msg.into())
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::Unauthorized(ErrorCode::UNAUTHORIZED, msg.into())
    }

    pub fn unauthorized_with(code: ErrorCode, msg: impl Into<String>) -> Self {
        Self::Unauthorized(code, msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(ErrorCode::FORBIDDEN, msg.into())
    }

    pub fn forbidden_with(code: ErrorCode, msg: impl Into<String>) -> Self {
        Self::Forbidden(code, msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(ErrorCode::CONFLICT, msg.into())
    }

    pub fn gone(msg: impl Into<String>) -> Self {
        Self::Gone(ErrorCode::GONE, msg.into())
    }

    pub fn payload_too_large(msg: impl Into<String>) -> Self {
        Self::PayloadTooLarge(ErrorCode::PAYLOAD_TOO_LARGE, msg.into())
    }

    pub fn unprocessable_entity(msg: impl Into<String>) -> Self {
        Self::UnprocessableEntity(ErrorCode::UNPROCESSABLE_ENTITY, msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(AnyhowError::msg(msg.into()))
    }

    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self::ServiceUnavailable(ErrorCode::SERVICE_UNAVAILABLE, msg.into())
    }

    // ── Domain-specific constructors ────────────────────────────────────

    pub fn invalid_name(name: &str) -> Self {
        Self::BadRequest(ErrorCode::INVALID_NAME, format!("Invalid {}", name))
    }

    pub fn already_exists(resource: &str) -> Self {
        Self::Conflict(
            ErrorCode::ALREADY_EXISTS,
            format!("{} already exists", resource),
        )
    }

    pub fn not_found(resource: &str) -> Self {
        Self::NotFound(ErrorCode::NOT_FOUND, format!("{} not found", resource))
    }

    /// Like [`not_found`] but takes a fully-formed message instead of
    /// appending " not found".
    pub fn not_found_msg(msg: impl Into<String>) -> Self {
        Self::NotFound(ErrorCode::NOT_FOUND, msg.into())
    }

    /// Internal-server-error for "<resource> data inconsistency" — a
    /// referential-integrity violation discovered at request time
    /// (e.g. a build row with no derivation row). Maps to HTTP 500 with the
    /// generic `internal` code, but is logged at warn level via the
    /// `DataInconsistency` variant since the cause is most often a transient
    /// race against concurrent deletion rather than a real server bug.
    pub fn data_inconsistency(resource: &str) -> Self {
        Self::DataInconsistency(format!("{} data inconsistency", resource))
    }

    pub fn invalid_credentials() -> Self {
        Self::Unauthorized(
            ErrorCode::INVALID_CREDENTIALS,
            "Invalid credentials".to_string(),
        )
    }

    pub fn oauth_disabled() -> Self {
        Self::BadRequest(
            ErrorCode::OAUTH_DISABLED,
            "OAuth login is disabled".to_string(),
        )
    }

    pub fn oauth_required() -> Self {
        Self::Unauthorized(
            ErrorCode::OAUTH_REQUIRED,
            "Please login via OAuth".to_string(),
        )
    }

    pub fn registration_disabled() -> Self {
        Self::BadRequest(
            ErrorCode::REGISTRATION_DISABLED,
            "Registration is disabled".to_string(),
        )
    }

    pub fn invalid_email() -> Self {
        Self::BadRequest(ErrorCode::INVALID_EMAIL, "Invalid Email".to_string())
    }

    pub fn failed_to_generate_token() -> Self {
        Self::Internal(AnyhowError::msg(format!(
            "[{}] Failed to generate token",
            ErrorCode::TOKEN_GENERATION_FAILED
        )))
    }

    pub fn failed_to_update_user() -> Self {
        Self::Internal(AnyhowError::msg(format!(
            "[{}] Failed to update user",
            ErrorCode::USER_UPDATE_FAILED
        )))
    }

    pub fn invalid_oauth_code() -> Self {
        Self::BadRequest(
            ErrorCode::INVALID_OAUTH_CODE,
            "Invalid OAuth Code".to_string(),
        )
    }

    pub fn failed_ssh_key_generation() -> Self {
        Self::Internal(AnyhowError::msg(format!(
            "[{}] Failed to generate SSH key",
            ErrorCode::SSH_KEY_GENERATION_FAILED
        )))
    }

    pub fn invalid_password(reason: String) -> Self {
        Self::BadRequest(
            ErrorCode::INVALID_PASSWORD,
            format!("Invalid password: {}", reason),
        )
    }

    pub fn invalid_username(reason: String) -> Self {
        Self::BadRequest(
            ErrorCode::INVALID_USERNAME,
            format!("Invalid username: {}", reason),
        )
    }

    /// Constructor for INSERT/UPDATE sites where a unique index is the
    /// source of truth for collision detection.
    pub(crate) fn from_db_err(err: DbErr, label: &str) -> Self {
        if is_unique_violation(&err) {
            return Self::already_exists(label);
        }
        Self::from(err)
    }
}

/// Returns `Forbidden` when the user is not a superuser. Use at the top of
/// admin handlers.
pub fn require_superuser(user: &gradient_core::types::MUser) -> Result<(), WebError> {
    if user.superuser {
        Ok(())
    } else {
        Err(WebError::Forbidden(
            ErrorCode::SUPERUSER_REQUIRED,
            "superuser required".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbErr, RuntimeErr};

    #[test]
    fn from_db_err_passes_through_non_db_errors() {
        let err = DbErr::Custom("boom".into());
        let mapped = WebError::from_db_err(err, "Anything");
        assert!(matches!(mapped, WebError::Internal(_)));
    }

    #[test]
    fn from_db_err_passes_through_query_string_errors() {
        let err = DbErr::Query(RuntimeErr::Internal("nope".into()));
        let mapped = WebError::from_db_err(err, "Cache Name");
        assert!(matches!(mapped, WebError::Internal(_)));
    }

    #[test]
    fn from_db_err_record_not_found_is_internal() {
        let err = DbErr::RecordNotFound("nothing".into());
        let mapped = WebError::from_db_err(err, "User");
        assert!(matches!(mapped, WebError::Internal(_)));
    }
}
