/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /admin/github-app/manifest`, `GET /admin/github-app/callback`,
//! `GET /admin/github-app/credentials`.

use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::{Extension, Json};
use gradient_types::{BaseResponse, MUser};
use gradient_core::ServerState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::warn;

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
        return Err(WebError::bad_request("host must not be empty"));
    }
    let ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok {
        return Err(WebError::bad_request(
            "host must contain only alphanumerics, '.' and '-'",
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

    let manifest =
        gradient_ci::github_app_manifest::build_manifest(&state.config.server.serve_url);
    let token = gradient_ci::manifest_state::issue_state(&state.manifest_state, user.id);
    let post_url = gradient_ci::github_app_manifest::manifest_post_url(&host, &token);

    Ok(ok_json(ManifestResponse {
        manifest,
        post_url,
        state: token,
    }))
}

/// Unauthenticated callback target for GitHub's manifest redirect.
///
/// GitHub redirects the operator's browser here from `https://github.com/...`
/// after the manifest is confirmed; that cross-site navigation never carries
/// our `Authorization: Bearer …` header, so the route cannot live behind the
/// usual auth middleware.
///
/// CSRF/identity is recovered from the one-shot `state` token that was issued
/// (and bound to a superuser) at `/admin/github-app/manifest`.
pub async fn callback(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<CallbackQuery>,
) -> WebResult<Redirect> {
    let host = q.host.unwrap_or_else(default_host);
    validate_host(&host)?;

    let Some(user_id) =
        gradient_ci::manifest_state::validate_and_consume(&state.manifest_state, &q.state)
    else {
        return Err(WebError::bad_request("manifest state invalid or expired"));
    };

    let creds = gradient_ci::github_app_manifest::exchange_code(&state.http, &host, &q.code)
        .await
        .map_err(|e| {
            warn!(error = %e, "github manifest exchange failed");
            WebError::internal(format!("github exchange failed: {e}"))
        })?;

    gradient_ci::manifest_state::store_credentials(
        &state.pending_credentials,
        user_id,
        creds,
    );

    Ok(Redirect::to("/admin/github-app?ready=1"))
}

pub async fn credentials(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<gradient_ci::github_app_manifest::ManifestResult>>> {
    require_superuser(&user)?;

    let creds =
        gradient_ci::manifest_state::take_credentials(&state.pending_credentials, user.id)
            .ok_or_else(|| WebError::not_found_msg("Pending credentials"))?;

    Ok(ok_json(creds))
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
