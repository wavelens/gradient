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
mod trigger;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use core::ci::{
    ForgeType, IntegrationKind, decrypt_webhook_secret, verify_gitea_signature,
    verify_github_signature,
};
use core::types::input::load_secret;
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use tracing::{debug, warn};

use events::ParsedPushEvent;
use trigger::handle_github_installation;

// ── GitHub App webhook ─────────────────────────────────────────────────────

/// `POST /api/v1/hooks/github` — receives all events from the GitHub App.
pub async fn github_app_webhook(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(github_app) = state.cli.github_app_config() else {
        warn!(
            "GitHub App webhook received but GitHub App is not fully configured \
             (requires GRADIENT_GITHUB_APP_ID, GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE, \
             and GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE)"
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let secret = load_secret(&github_app.webhook_secret_file);

    let sig_header = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !verify_github_signature(secret.expose(), sig_header, &body) {
        warn!("GitHub App webhook: invalid signature");
        return StatusCode::UNAUTHORIZED;
    }

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    debug!(event, "GitHub App webhook received");

    match event {
        "ping" => StatusCode::OK,
        "push" => {
            if let Some(event) = ParsedPushEvent::from_github(&body) {
                event.trigger(&state).await;
            }
            StatusCode::OK
        }
        "installation" | "installation_repositories" => {
            handle_github_installation(&state, &body).await;
            StatusCode::OK
        }
        _ => StatusCode::OK,
    }
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
) -> impl IntoResponse {
    let Some(forge_type) = ForgeType::from_path_segment(&forge) else {
        warn!(forge = %forge, "Unknown forge path segment");
        return StatusCode::NOT_FOUND;
    };
    // GitHub deliveries go through the App webhook at `/hooks/github`, not here.
    if matches!(forge_type, ForgeType::GitHub) {
        return StatusCode::NOT_FOUND;
    }

    let org = match EOrganization::find()
        .filter(COrganization::Name.eq(org_name.as_str()))
        .one(&state.db)
        .await
    {
        Ok(Some(o)) => o,
        Ok(None) => return StatusCode::NOT_FOUND,
        Err(e) => {
            warn!(error = %e, org = %org_name, "DB error looking up organization for forge webhook");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // A single inbound integration can serve Gitea/Forgejo/GitLab — the frontend
    // chooses which forge's webhook URL to copy. Signature verification uses the
    // `forge` path segment to pick the HMAC scheme.
    let integration = match EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .filter(CIntegration::Kind.eq(IntegrationKind::Inbound.as_i16()))
        .filter(CIntegration::Name.eq(integration_name.as_str()))
        .one(&state.db)
        .await
    {
        Ok(Some(i)) => i,
        Ok(None) => {
            warn!(org = %org_name, %forge, integration = %integration_name, "Integration not found");
            return StatusCode::NOT_FOUND;
        }
        Err(e) => {
            warn!(error = %e, "DB error looking up integration for forge webhook");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let Some(ref encrypted_secret) = integration.secret else {
        warn!(integration_id = %integration.id, "Integration has no secret configured");
        return StatusCode::SERVICE_UNAVAILABLE;
    };

    let plaintext_secret =
        match decrypt_webhook_secret(&state.cli.crypt_secret_file, encrypted_secret) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, integration_id = %integration.id, "Failed to decrypt integration secret");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        };

    if !verify_forge_signature(forge_type, plaintext_secret.expose(), &headers, &body) {
        warn!(org = %org_name, forge = %forge, integration = %integration_name, "Forge webhook: invalid signature");
        return StatusCode::UNAUTHORIZED;
    }

    let event = match forge_type {
        ForgeType::Gitea | ForgeType::Forgejo => ParsedPushEvent::from_gitea(&body),
        ForgeType::GitLab => ParsedPushEvent::from_gitlab(&body),
        ForgeType::GitHub => return StatusCode::NOT_FOUND, // unreachable; checked above
    };
    if let Some(event) = event {
        event.trigger(&state).await;
    }

    StatusCode::OK
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
        assert!(!verify_forge_signature(ForgeType::GitLab, "s3cret", &h, b""));
    }

    #[test]
    fn gitlab_rejects_missing_token() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature(ForgeType::GitLab, "s3cret", &h, b""));
    }

    #[test]
    fn gitea_rejects_missing_signature() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature(ForgeType::Gitea, "s3cret", &h, b"body"));
    }
}
