/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub const SCIM_CONTENT_TYPE: &str = "application/scim+json";

#[derive(Debug)]
pub struct ScimError {
    pub status: StatusCode,
    pub scim_type: Option<&'static str>,
    pub detail: String,
}

pub type ScimResult<T> = Result<T, ScimError>;

impl ScimError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            scim_type: None,
            detail: detail.into(),
        }
    }
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, detail)
    }
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }
    pub fn bad_request(scim_type: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            scim_type: Some(scim_type),
            detail: detail.into(),
        }
    }
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            scim_type: Some("uniqueness"),
            detail: detail.into(),
        }
    }
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, detail)
    }
}

#[derive(Serialize)]
struct ScimErrorBody {
    schemas: [&'static str; 1],
    status: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "scimType")]
    scim_type: Option<&'static str>,
    detail: String,
}

impl IntoResponse for ScimError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(detail = %self.detail, "SCIM error");
        }

        let body = ScimErrorBody {
            schemas: ["urn:ietf:params:scim:api:messages:2.0:Error"],
            status: self.status.as_u16().to_string(),
            scim_type: self.scim_type,
            detail: self.detail,
        };
        (
            self.status,
            [(header::CONTENT_TYPE, SCIM_CONTENT_TYPE)],
            Json(body),
        )
            .into_response()
    }
}

impl From<sea_orm::DbErr> for ScimError {
    fn from(e: sea_orm::DbErr) -> Self {
        ScimError::internal(format!("database error: {e}"))
    }
}
