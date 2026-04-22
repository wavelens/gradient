/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /admin/github-app/manifest`, `GET /admin/github-app/callback`,
//! `GET /admin/github-app/credentials`.

use crate::error::{WebError, WebResult, require_superuser};
use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::{Extension, Json};
use core::types::{BaseResponse, MUser, ServerState};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, Default, Debug)]
pub struct ManifestRequest {
    /// Defaults to `github.com` if omitted.
    pub host: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct ManifestResponse {
    pub manifest: serde_json::Value,
    pub post_url: String,
    pub state: String,
}

#[derive(Deserialize, Debug)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
    pub host: Option<String>,
}

fn default_host() -> String {
    "github.com".to_string()
}

pub fn validate_host(host: &str) -> Result<(), WebError> {
    if host.is_empty() {
        return Err(WebError::BadRequest("host must not be empty".into()));
    }
    let ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok {
        return Err(WebError::BadRequest(
            "host must contain only alphanumerics, '.' and '-'".into(),
        ));
    }
    Ok(())
}

pub async fn request_manifest(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<ManifestRequest>,
) -> WebResult<Json<BaseResponse<ManifestResponse>>> {
    require_superuser(&user)?;

    let host = body.host.unwrap_or_else(default_host);
    validate_host(&host)?;

    let manifest = core::ci::github_app_manifest::build_manifest(&state.cli.serve_url);
    let token = core::ci::manifest_state::issue_state(&state.manifest_state);
    let post_url = core::ci::github_app_manifest::manifest_post_url(&host, &token);

    Ok(Json(BaseResponse {
        error: false,
        message: ManifestResponse {
            manifest,
            post_url,
            state: token,
        },
    }))
}

pub async fn callback(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(q): Query<CallbackQuery>,
) -> WebResult<Redirect> {
    require_superuser(&user)?;

    let host = q.host.unwrap_or_else(default_host);
    validate_host(&host)?;

    if !core::ci::manifest_state::validate_and_consume(&state.manifest_state, &q.state) {
        return Err(WebError::BadRequest(
            "manifest state invalid or expired".into(),
        ));
    }

    let creds = core::ci::github_app_manifest::exchange_code(&host, &q.code)
        .await
        .map_err(|e| {
            WebError::InternalServerError(format!("github exchange failed: {e}"))
        })?;

    core::ci::manifest_state::store_credentials(&state.pending_credentials, user.id, creds);

    Ok(Redirect::to("/admin/github-app?ready=1"))
}

pub async fn credentials(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<core::ci::github_app_manifest::ManifestResult>>> {
    require_superuser(&user)?;

    let creds = core::ci::manifest_state::take_credentials(&state.pending_credentials, user.id)
        .ok_or_else(|| WebError::NotFound("Pending credentials".into()))?;

    Ok(Json(BaseResponse {
        error: false,
        message: creds,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_host_accepts_normal_hosts() {
        assert!(validate_host("github.com").is_ok());
        assert!(validate_host("ghe.example.com").is_ok());
        assert!(validate_host("ghe-1.example.com").is_ok());
    }

    #[test]
    fn validate_host_rejects_path_chars() {
        assert!(validate_host("evil.com/inject").is_err());
        assert!(validate_host("evil.com?x=1").is_err());
        assert!(validate_host("evil com").is_err());
    }

    #[test]
    fn validate_host_rejects_empty() {
        assert!(validate_host("").is_err());
    }
}
