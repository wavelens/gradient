/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::AdminRefusal;
use crate::error::{WebError, WebResult};
use crate::extractors::{JsonBody, Query};
use crate::helpers::ok_json;
use crate::refusal::SharedRefusal;
use anyhow::Context;
use axum::extract::State;
use axum::response::Redirect;
use axum::{Extension, Json};
pub use gradient_api_types::{GithubAppManifestRequest, GithubAppManifestResponse};
use gradient_core::ServerState;
use gradient_types::{BaseResponse, MUser};
use serde::Deserialize;
use std::sync::Arc;

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

pub fn clean_github_enterprise_host(raw: &str) -> Result<String, WebError> {
    let trimmed = raw.trim();
    let without_scheme = ["https://", "http://"]
        .iter()
        .find_map(|scheme| {
            trimmed
                .get(..scheme.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(scheme))
                .map(|_| &trimmed[scheme.len()..])
        })
        .unwrap_or(trimmed);
    let host = without_scheme.trim_end_matches('/');

    if host.is_empty() {
        return Err(SharedRefusal::EmptyField("host").into());
    }

    let ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');

    if !ok {
        return Err(AdminRefusal::InvalidHostCharacters.into());
    }

    Ok(host.to_owned())
}

pub fn clean_github_organization(raw: Option<&str>) -> Result<Option<String>, WebError> {
    let Some(organization) = raw.map(str::trim).filter(|o| !o.is_empty()) else {
        return Ok(None);
    };
    let ok = organization
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !ok {
        return Err(AdminRefusal::InvalidOrganizationCharacters.into());
    }
    Ok(Some(organization.to_owned()))
}

#[utoipa::path(
    post,
    path = "/github-app/manifest",
    operation_id = "createGithubAppManifest",
    tag = "admin",
    summary = "Build GitHub App manifest",
    description = "Build a GitHub App manifest and prefill this server's URL, permissions and events. Set ``organization`` to place the app in that GitHub organization instead of the admin's personal account. Return a one-shot CSRF state token and the GitHub URL that the browser should POST the manifest to.\n\nRequests require `superuser`. Matching calls to `GET /admin/github-app/callback` use the state token once. State tokens expire after 10 minutes.",
    request_body = GithubAppManifestRequest,
    responses((status = OK, description = "Manifest, post URL, and CSRF state", body = GithubAppManifestResponse), WebError)
)]
pub async fn request_manifest(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    JsonBody(body): JsonBody<GithubAppManifestRequest>,
) -> WebResult<Json<BaseResponse<GithubAppManifestResponse>>> {
    let host = clean_github_enterprise_host(&body.host.unwrap_or_else(default_host))?;
    let organization = clean_github_organization(body.organization.as_deref())?;

    let manifest = gradient_ci::github_app_manifest::build_manifest(&state.config.server.serve_url);
    let token = state.memory.manifest_state.issue_state(user.id).await;
    let post_url =
        gradient_ci::github_app_manifest::manifest_post_url(&host, organization.as_deref(), &token);

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
    description = "Receive the redirect from GitHub for the manifest that the admin submitted. Verify the ``state`` token. Exchange temporary ``code`` for the persistent credentials of the App. Store them in a one-shot in-memory store per superuser that started the manifest flow. Redirect the browser to ``/admin/git-host-connections?ready=1``.\n\n**Unauthenticated** - GitHub redirects the browser of the admin from `https://github.com/...` to this location, but that cross-site navigation sends no bearer token. CSRF and identity binding come from a single-use `state` token that `POST /admin/github-app/manifest` issues, and that token ties to the superuser who started it.",
    params(CallbackQuery),
    responses((status = FOUND, description = "Successful responses redirect to `/admin/git-host-connections?ready=1`."), WebError)
)]
pub async fn callback(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<CallbackQuery>,
) -> WebResult<Redirect> {
    let host = clean_github_enterprise_host(&q.host.unwrap_or_else(default_host))?;

    let Some(user_id) = state
        .memory
        .manifest_state
        .validate_and_consume(&q.state)
        .await
    else {
        return Err(AdminRefusal::InvalidManifestState.into());
    };

    let creds = gradient_ci::github_app_manifest::exchange_code(
        &state.admin_configured_endpoint_http_client,
        &host,
        &q.code,
    )
    .await
    .context("run github exchange")?;

    state
        .memory
        .pending_credentials
        .store_credentials(user_id, creds)
        .await;

    Ok(Redirect::to("/admin/git-host-connections?ready=1"))
}

#[utoipa::path(
    post,
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
    fn clean_github_enterprise_host_accepts_normal_hosts() {
        assert!(clean_github_enterprise_host("github.com").is_ok());
        assert!(clean_github_enterprise_host("ghe.example.com").is_ok());
        assert!(clean_github_enterprise_host("ghe-1.example.com").is_ok());
    }

    #[test]
    fn clean_github_enterprise_host_removes_scheme_spaces_and_trailing_slashes() {
        for raw in [
            " https://ghe.example.com/ ",
            "HTTP://ghe.example.com",
            "ghe.example.com//",
        ] {
            assert_eq!(
                clean_github_enterprise_host(raw).unwrap(),
                "ghe.example.com"
            );
        }
    }

    #[test]
    fn clean_github_enterprise_host_rejects_path_chars() {
        assert!(clean_github_enterprise_host("evil.com/inject").is_err());
        assert!(clean_github_enterprise_host("evil.com?x=1").is_err());
        assert!(clean_github_enterprise_host("evil com").is_err());
    }

    #[test]
    fn an_empty_organization_means_the_personal_account() {
        assert_eq!(clean_github_organization(None).unwrap(), None);
        assert_eq!(clean_github_organization(Some("  ")).unwrap(), None);
        assert_eq!(
            clean_github_organization(Some(" acme-corp "))
                .unwrap()
                .as_deref(),
            Some("acme-corp")
        );
    }

    #[test]
    fn an_organization_with_path_characters_is_refused() {
        assert!(clean_github_organization(Some("acme/../evil")).is_err());
        assert!(clean_github_organization(Some("acme?x=1")).is_err());
    }

    #[test]
    fn clean_github_enterprise_host_rejects_empty() {
        assert!(clean_github_enterprise_host("").is_err());
        assert!(clean_github_enterprise_host("https://").is_err());
    }
}
