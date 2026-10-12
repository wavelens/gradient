/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::AdminRefusal;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::refusal::SharedRefusal;
use anyhow::Context;
use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::{BaseResponse, MUser};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, Default, Debug, utoipa::ToSchema)]
pub struct GithubAppManifestRequest {
    pub host: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GithubAppManifestResponse {
    pub manifest: serde_json::Value,
    pub post_url: String,
    pub state: String,
}

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
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
        return Err(SharedRefusal::EmptyField("host").into());
    }

    let ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');

    if !ok {
        return Err(AdminRefusal::InvalidHostCharacters.into());
    }

    Ok(())
}

#[utoipa::path(
    post,
    path = "/github-app/manifest",
    operation_id = "createGithubAppManifest",
    tag = "admin",
    summary = "Build GitHub App manifest",
    description = "Builds a GitHub App manifest pre-filled with this server's URLs,\npermissions, and events, and returns a one-shot CSRF state token plus\nthe GitHub URL the browser should POST the manifest to.\n\nRequires `superuser`. The state token is consumed by the matching\n`GET /admin/github-app/callback` invocation; it expires after 10\nminutes.",
    request_body = GithubAppManifestRequest,
    responses((status = OK, description = "Manifest, post URL, and CSRF state", body = GithubAppManifestResponse), WebError)
)]
pub async fn request_manifest(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<GithubAppManifestRequest>,
) -> WebResult<Json<BaseResponse<GithubAppManifestResponse>>> {
    let host = body.host.unwrap_or_else(default_host);
    validate_host(&host)?;

    let manifest = gradient_ci::github_app_manifest::build_manifest(&state.config.server.serve_url);
    let token = state.memory.manifest_state.issue_state(user.id).await;
    let post_url = gradient_ci::github_app_manifest::manifest_post_url(&host, &token);

    Ok(ok_json(GithubAppManifestResponse {
        manifest,
        post_url,
        state: token,
    }))
}

#[utoipa::path(
    get,
    path = "/admin/github-app/callback",
    operation_id = "githubAppManifestCallback",
    tag = "admin",
    summary = "GitHub App manifest callback",
    description = "Receives the redirect from GitHub after the operator submits the\nmanifest. Validates the `state` token, exchanges the temporary `code`\nfor the App's permanent credentials, stashes them in a one-shot\nin-memory store per superuser who initiated the manifest\nflow, and redirects the browser to `/admin/github-app?ready=1`.\n\n**Unauthenticated** - GitHub redirects the operator's browser here\nfrom `https://github.com/...` and that cross-site navigation does not\ncarry a bearer token. CSRF and identity binding are provided by the\none-shot `state` token issued at `POST /admin/github-app/manifest`,\nwhich is bound to the initiating superuser at issuance.",
    params(CallbackQuery),
    responses((status = FOUND, description = "Redirect to `/admin/github-app?ready=1` on success."), WebError)
)]
pub async fn callback(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<CallbackQuery>,
) -> WebResult<Redirect> {
    let host = q.host.unwrap_or_else(default_host);
    validate_host(&host)?;

    let Some(user_id) = state
        .memory
        .manifest_state
        .validate_and_consume(&q.state)
        .await
    else {
        return Err(AdminRefusal::InvalidManifestState.into());
    };

    let creds = gradient_ci::github_app_manifest::exchange_code(&state.http, &host, &q.code)
        .await
        .context("run github exchange")?;

    state
        .memory
        .pending_credentials
        .store_credentials(user_id, creds)
        .await;

    Ok(Redirect::to("/admin/github-app?ready=1"))
}

#[utoipa::path(
    get,
    path = "/github-app/credentials",
    operation_id = "getGithubAppCredentials",
    tag = "admin",
    summary = "Fetch pending GitHub App credentials",
    description = "One-shot retrieval of the credentials produced by the most recent\nsuccessful manifest exchange. Calling this endpoint **consumes** the\npending credentials; a second call returns `404`. Pending entries\nalso expire 10 minutes after creation.\n\nRequires `superuser`.",
    responses((status = OK, description = "GitHub App credentials (returned exactly once)", body = gradient_ci::github_app_manifest::ManifestResult), WebError)
)]
pub async fn credentials(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<gradient_ci::github_app_manifest::ManifestResult>>> {
    let creds = state
        .memory
        .pending_credentials
        .take_credentials(user.id)
        .await
        .ok_or(AdminRefusal::NoPendingCredentials)?;

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
