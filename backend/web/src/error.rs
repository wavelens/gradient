/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
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
            WebError::Database(err) => write!(f, "Database Error: {}", err),
            WebError::Validation(msg) => write!(f, "Validation Error: {}", msg),
            WebError::Authentication(msg) => write!(f, "Authentication Error: {}", msg),
        }
    }
}

impl std::error::Error for WebError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WebError::Database(err) => Some(err),
            _ => None,
        }
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
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error".to_string())
            }
            WebError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            WebError::Authentication(msg) => (StatusCode::UNAUTHORIZED, msg),
        };

        let body = Json(BaseResponse {
            error: true,
            message: error_message,
        });

        (status, body).into_response()
    }
}

impl From<DbErr> for WebError {
    fn from(err: DbErr) -> Self {
        WebError::Database(err)
    }
}

impl From<JsonRejection> for WebError {
    fn from(rejection: JsonRejection) -> Self {
        WebError::BadRequest(format!("Invalid JSON: {}", rejection))
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
}