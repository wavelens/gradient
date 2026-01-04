/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Error as AnyhowError;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use core::types::BaseResponse;
use sea_orm::DbErr;
use std::fmt;

#[derive(Debug)]
pub enum WebError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    UnprocessableEntity(String),
    InternalServerError(String),
    Database(DbErr),
    Validation(String),
    Authentication(String),
    InputValidation(core::input::InputError),
    JsonParsing(JsonRejection),
    Internal(AnyhowError),
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebError::BadRequest(msg) => write!(f, "Bad Request: {}", msg),
            WebError::Unauthorized(msg) => write!(f, "Unauthorized: {}", msg),
            WebError::Forbidden(msg) => write!(f, "Forbidden: {}", msg),
            WebError::NotFound(msg) => write!(f, "Not Found: {}", msg),
            WebError::Conflict(msg) => write!(f, "Conflict: {}", msg),
            WebError::UnprocessableEntity(msg) => write!(f, "Unprocessable Entity: {}", msg),
            WebError::InternalServerError(msg) => write!(f, "Internal Server Error: {}", msg),
            WebError::Database(err) => write!(f, "Database error: {}", err),
            WebError::Validation(msg) => write!(f, "Validation error: {}", msg),
            WebError::Authentication(msg) => write!(f, "Authentication error: {}", msg),
            WebError::InputValidation(err) => write!(f, "Input validation error: {}", err),
            WebError::JsonParsing(err) => write!(f, "JSON parsing error: {}", err),
            WebError::Internal(err) => write!(f, "Internal error: {}", err),
        }
    }
}

impl std::error::Error for WebError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WebError::Database(err) => Some(err),
            WebError::InputValidation(err) => Some(err),
            WebError::JsonParsing(err) => Some(err),
            WebError::Internal(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

// Manual From implementations to avoid naming conflicts with core crate
impl From<DbErr> for WebError {
    fn from(err: DbErr) -> Self {
        WebError::Database(err)
    }
}

impl From<core::input::InputError> for WebError {
    fn from(err: core::input::InputError) -> Self {
        WebError::InputValidation(err)
    }
}

impl From<JsonRejection> for WebError {
    fn from(err: JsonRejection) -> Self {
        WebError::JsonParsing(err)
    }
}

impl From<AnyhowError> for WebError {
    fn from(err: AnyhowError) -> Self {
        WebError::Internal(err)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            WebError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            WebError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            WebError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            WebError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            WebError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            WebError::UnprocessableEntity(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            WebError::InternalServerError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            WebError::Database(err) => {
                tracing::error!("Database error: {}", err);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database error".to_string(),
                )
            }
            WebError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            WebError::Authentication(msg) => (StatusCode::UNAUTHORIZED, msg),
            WebError::InputValidation(err) => (StatusCode::BAD_REQUEST, err.to_string()),
            WebError::JsonParsing(err) => {
                (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", err))
            }
            WebError::Internal(err) => {
                tracing::error!("Internal error: {}", err);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
        };

        let body = Json(BaseResponse {
            error: true,
            message: error_message,
        });

        (status, body).into_response()
    }
}

pub type WebResult<T> = Result<T, WebError>;

// Helper functions for common error scenarios
impl WebError {
    pub fn invalid_name(name: &str) -> Self {
        WebError::BadRequest(format!("Invalid {}", name))
    }

    pub fn already_exists(resource: &str) -> Self {
        WebError::Conflict(format!("{} already exists", resource))
    }

    pub fn not_found(resource: &str) -> Self {
        WebError::NotFound(format!("{} not found", resource))
    }

    pub fn invalid_credentials() -> Self {
        WebError::Unauthorized("Invalid credentials".to_string())
    }

    pub fn oauth_disabled() -> Self {
        WebError::BadRequest("OAuth login is disabled".to_string())
    }

    pub fn oauth_required() -> Self {
        WebError::Unauthorized("Please login via OAuth".to_string())
    }

    pub fn registration_disabled() -> Self {
        WebError::BadRequest("Registration is disabled".to_string())
    }

    pub fn invalid_email() -> Self {
        WebError::BadRequest("Invalid Email".to_string())
    }

    pub fn failed_to_generate_token() -> Self {
        WebError::InternalServerError("Failed to generate token".to_string())
    }

    pub fn failed_to_update_user() -> Self {
        WebError::InternalServerError("Failed to update user".to_string())
    }

    pub fn invalid_oauth_code() -> Self {
        WebError::BadRequest("Invalid OAuth Code".to_string())
    }

    pub fn failed_ssh_key_generation() -> Self {
        WebError::InternalServerError("Failed to generate SSH key".to_string())
    }

    pub fn invalid_password(reason: String) -> Self {
        WebError::BadRequest(format!("Invalid password: {}", reason))
    }

    pub fn invalid_username(reason: String) -> Self {
        WebError::BadRequest(format!("Invalid username: {}", reason))
    }
}
