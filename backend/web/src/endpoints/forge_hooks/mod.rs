/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Incoming forge webhook endpoints.
//!
//! These routes are **unauthenticated** — they verify callers via HMAC
//! signatures or token headers instead of JWTs.
//!
//! | Endpoint                                              | Forge          | Auth method             |
//! |-------------------------------------------------------|----------------|-------------------------|
//! | `POST /hooks/github`                                  | GitHub App     | `X-Hub-Signature-256`   |
//! | `POST /hooks/{forge}/{org}/{integration_name}`        | Gitea/Forgejo/GitLab | per-integration secret |

mod events;
mod response;
mod trigger;

pub use response::{QueuedEvaluation, SkippedProject, WebhookResponse, WebhookTriggerOutcome};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use core::ci::{
    ForgeType, IntegrationKind, decrypt_webhook_secret, verify_gitea_signature,
    verify_github_signature,
};
use core::types::input::load_secret;
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::error::{WebError, WebResult};

use events::ParsedPushEvent;
use trigger::handle_github_installation;

// ── GitHub App webhook ─────────────────────────────────────────────────────

/// `POST /api/v1/hooks/github` — receives all events from the GitHub App.
pub async fn github_app_webhook(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let Some(github_app) = state.cli.github_app_config() else {
        warn!(
            "GitHub App webhook received but GitHub App is not fully configured \
             (requires GRADIENT_GITHUB_APP_ID, GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE, \
             and GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE)"
        );
        return Err(WebError::ServiceUnavailable(
            "github app integration not configured".into(),
        ));
    };
    let secret = load_secret(&github_app.webhook_secret_file);

    let sig_header = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !verify_github_signature(secret.expose(), sig_header, &body) {
        warn!("GitHub App webhook: invalid signature");
        return Err(WebError::Unauthorized("invalid webhook signature".into()));
    }

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    debug!(event = %event, "GitHub App webhook received");

    let response = match event.as_str() {
        "push" => {
            let Some(parsed) = ParsedPushEvent::from_github(&body) else {
                return Err(WebError::BadRequest("malformed webhook payload".into()));
            };
            let urls = parsed.repository_urls.clone();
            let outcome = parsed.trigger(&state).await;
            WebhookResponse {
                event: "push".to_string(),
                repository_urls: urls,
                projects_scanned: outcome.projects_scanned,
                queued: outcome.queued,
                skipped: outcome.skipped,
            }
        }
        "installation" | "installation_repositories" => {
            handle_github_installation(&state, &body).await;
            WebhookResponse::empty(&event)
        }
        other => WebhookResponse::empty(other),
    };

    Ok(Json(BaseResponse {
        error: false,
        message: response,
    }))
}

// ── Generic forge webhook ──────────────────────────────────────────────────

/// `POST /api/v1/hooks/{forge}/{org_name}/{integration_name}` — receives push
/// events from a named inbound integration.
///
/// The `forge` path segment is one of: `gitea`, `forgejo`, `gitlab`.
pub async fn forge_webhook(
    State(state): State<Arc<ServerState>>,
    Path((forge, org_name, integration_name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let Some(forge_type) = ForgeType::from_path_segment(&forge) else {
        warn!(forge = %forge, "Unknown forge path segment");
        return Err(WebError::NotFound("integration not found".into()));
    };
    if matches!(forge_type, ForgeType::GitHub) {
        return Err(WebError::BadRequest("unsupported forge".into()));
    }

    let org = EOrganization::find()
        .filter(COrganization::Name.eq(org_name.as_str()))
        .one(&state.db)
        .await
        .map_err(|e| {
            warn!(error = %e, org = %org_name, "DB error looking up organization for forge webhook");
            WebError::InternalServerError("internal error".into())
        })?
        .ok_or_else(|| WebError::NotFound("integration not found".into()))?;

    let integration = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .filter(CIntegration::Kind.eq(IntegrationKind::Inbound.as_i16()))
        .filter(CIntegration::Name.eq(integration_name.as_str()))
        .one(&state.db)
        .await
        .map_err(|e| {
            warn!(error = %e, "DB error looking up integration for forge webhook");
            WebError::InternalServerError("internal error".into())
        })?
        .ok_or_else(|| {
            warn!(org = %org_name, %forge, integration = %integration_name, "Integration not found");
            WebError::NotFound("integration not found".into())
        })?;

    let encrypted_secret = integration.secret.as_ref().ok_or_else(|| {
        warn!(integration_id = %integration.id, "Integration has no secret configured");
        WebError::NotFound("integration not found".into())
    })?;

    let plaintext_secret =
        decrypt_webhook_secret(&state.cli.crypt_secret_file, encrypted_secret).map_err(|e| {
            warn!(error = %e, integration_id = %integration.id, "Failed to decrypt integration secret");
            WebError::InternalServerError("internal error".into())
        })?;

    if !verify_forge_signature(forge_type, plaintext_secret.expose(), &headers, &body) {
        warn!(org = %org_name, forge = %forge, integration = %integration_name, "Forge webhook: invalid signature");
        return Err(WebError::Unauthorized("invalid webhook signature".into()));
    }

    let parsed = match forge_type {
        ForgeType::Gitea | ForgeType::Forgejo => ParsedPushEvent::from_gitea(&body),
        ForgeType::GitLab => ParsedPushEvent::from_gitlab(&body),
        ForgeType::GitHub => unreachable!("checked above"),
    };
    let Some(parsed) = parsed else {
        return Err(WebError::BadRequest("malformed webhook payload".into()));
    };

    let urls = parsed.repository_urls.clone();
    let outcome = parsed.trigger(&state).await;

    Ok(Json(BaseResponse {
        error: false,
        message: WebhookResponse {
            event: "push".to_string(),
            repository_urls: urls,
            projects_scanned: outcome.projects_scanned,
            queued: outcome.queued,
            skipped: outcome.skipped,
        },
    }))
}

fn verify_forge_signature(
    forge: ForgeType,
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> bool {
    match forge {
        ForgeType::Gitea | ForgeType::Forgejo => {
            let sig = headers
                .get("X-Gitea-Signature")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            verify_gitea_signature(secret, sig, body)
        }
        ForgeType::GitLab => {
            let token = headers
                .get("X-Gitlab-Token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            token == secret
        }
        ForgeType::GitHub => {
            let sig = headers
                .get("X-Hub-Signature-256")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            verify_github_signature(secret, sig, body)
        }
    }
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn gitlab_matches_token_exactly() {
        let mut h = HeaderMap::new();
        h.insert("X-Gitlab-Token", HeaderValue::from_static("s3cret"));
        assert!(verify_forge_signature(ForgeType::GitLab, "s3cret", &h, b""));
    }

    #[test]
    fn gitlab_rejects_mismatched_token() {
        let mut h = HeaderMap::new();
        h.insert("X-Gitlab-Token", HeaderValue::from_static("wrong"));
        assert!(!verify_forge_signature(
            ForgeType::GitLab,
            "s3cret",
            &h,
            b""
        ));
    }

    #[test]
    fn gitlab_rejects_missing_token() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature(
            ForgeType::GitLab,
            "s3cret",
            &h,
            b""
        ));
    }

    #[test]
    fn gitea_rejects_missing_signature() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature(
            ForgeType::Gitea,
            "s3cret",
            &h,
            b"body"
        ));
    }
}
