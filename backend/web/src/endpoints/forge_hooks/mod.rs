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
//! | Endpoint                              | Forge          | Auth method             |
//! |---------------------------------------|----------------|-------------------------|
//! | `POST /hooks/github`                  | GitHub App     | `X-Hub-Signature-256`   |
//! | `POST /hooks/{forge}/{org}`           | Gitea/Forgejo/GitLab | per-org secret   |

mod events;
mod trigger;

use crate::error::{WebError, WebResult};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use core::ci::decrypt_webhook_secret;
use core::ci::{verify_gitea_signature, verify_github_signature};
use core::types::input::load_secret;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
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

/// `POST /api/v1/hooks/{forge}/{org_name}` — receives push events from
/// Gitea, Forgejo, or GitLab configured to send webhooks to Gradient.
///
/// The `forge` path segment is one of: `gitea`, `forgejo`, `gitlab`.
pub async fn forge_webhook(
    State(state): State<Arc<ServerState>>,
    Path((forge, org_name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
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

    let Some(ref encrypted_secret) = org.forge_webhook_secret else {
        warn!(org = %org_name, forge = %forge, "Forge webhook received but org has no forge_webhook_secret configured");
        return StatusCode::SERVICE_UNAVAILABLE;
    };

    let plaintext_secret =
        match decrypt_webhook_secret(&state.cli.crypt_secret_file, encrypted_secret) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, org = %org_name, "Failed to decrypt forge_webhook_secret");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        };

    let verified = verify_forge_signature(&forge, plaintext_secret.expose(), &headers, &body);
    if !verified {
        warn!(org = %org_name, forge = %forge, "Forge webhook: invalid signature");
        return StatusCode::UNAUTHORIZED;
    }

    let event = match forge.as_str() {
        "gitea" | "forgejo" | "github" => ParsedPushEvent::from_gitea(&body),
        "gitlab" => ParsedPushEvent::from_gitlab(&body),
        _ => None,
    };
    if let Some(event) = event {
        event.trigger(&state).await;
    }

    StatusCode::OK
}

fn verify_forge_signature(forge: &str, secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    match forge {
        "gitea" | "forgejo" => {
            let sig = headers
                .get("X-Gitea-Signature")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            verify_gitea_signature(secret, sig, body)
        }
        "gitlab" => {
            let token = headers
                .get("X-Gitlab-Token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            token == secret
        }
        "github" => {
            let sig = headers
                .get("X-Hub-Signature-256")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            verify_github_signature(secret, sig, body)
        }
        unknown => {
            warn!(forge = %unknown, "Unknown forge type in webhook path");
            false
        }
    }
}

// ── Org forge webhook secret management ───────────────────────────────────

#[cfg(test)]
mod verify_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn gitlab_matches_token_exactly() {
        let mut h = HeaderMap::new();
        h.insert("X-Gitlab-Token", HeaderValue::from_static("s3cret"));
        assert!(verify_forge_signature("gitlab", "s3cret", &h, b""));
    }

    #[test]
    fn gitlab_rejects_mismatched_token() {
        let mut h = HeaderMap::new();
        h.insert("X-Gitlab-Token", HeaderValue::from_static("wrong"));
        assert!(!verify_forge_signature("gitlab", "s3cret", &h, b""));
    }

    #[test]
    fn gitlab_rejects_missing_token() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature("gitlab", "s3cret", &h, b""));
    }

    #[test]
    fn unknown_forge_is_rejected() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature("bitbucket", "s3cret", &h, b""));
    }

    #[test]
    fn gitea_rejects_missing_signature() {
        let h = HeaderMap::new();
        assert!(!verify_forge_signature("gitea", "s3cret", &h, b"body"));
    }
}

/// Response for the forge webhook secret endpoint.
#[derive(serde::Serialize)]
pub struct ForgeWebhookSecretResponse {
    pub webhook_url: String,
    pub secret: String,
}

async fn load_forge_editable_org(
    state: &Arc<ServerState>,
    user_id: uuid::Uuid,
    org_name: &str,
) -> WebResult<MOrganization> {
    use super::projects::user_can_edit;

    let org = EOrganization::find()
        .filter(COrganization::Name.eq(org_name))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    if !user_can_edit(state, user_id, org.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to manage forge webhooks for this organization."
                .to_string(),
        ));
    }

    Ok(org)
}

/// `POST /api/v1/orgs/{organization}/forge-webhook-secret` — generates or
/// rotates the per-org forge webhook secret.
pub async fn post_forge_webhook_secret(
    State(state): State<Arc<ServerState>>,
    axum::Extension(user): axum::Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<axum::Json<BaseResponse<ForgeWebhookSecretResponse>>> {
    use core::ci::encrypt_webhook_secret;
    use rand::RngExt;

    let org = load_forge_editable_org(&state, user.id, &organization).await?;

    let mut secret_bytes = [0u8; 32];
    rand::rng().fill(&mut secret_bytes);
    let plaintext = hex::encode(secret_bytes);

    let encrypted =
        encrypt_webhook_secret(&state.cli.crypt_secret_file, &plaintext).map_err(|e| {
            WebError::InternalServerError(format!("Failed to encrypt webhook secret: {e}"))
        })?;

    let mut active = org.into_active_model();
    active.forge_webhook_secret = Set(Some(encrypted));
    active.update(&state.db).await?;

    let webhook_url = format!(
        "{}/api/v1/hooks/gitea/{}",
        state.cli.serve_url, organization
    );

    Ok(axum::Json(BaseResponse {
        error: false,
        message: ForgeWebhookSecretResponse {
            webhook_url,
            secret: plaintext,
        },
    }))
}

/// `DELETE /api/v1/orgs/{organization}/forge-webhook-secret` — removes the
/// per-org forge webhook secret.
pub async fn delete_forge_webhook_secret(
    State(state): State<Arc<ServerState>>,
    axum::Extension(user): axum::Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<axum::Json<BaseResponse<String>>> {
    let org = load_forge_editable_org(&state, user.id, &organization).await?;

    let mut active = org.into_active_model();
    active.forge_webhook_secret = Set(None);
    active.update(&state.db).await?;

    Ok(axum::Json(BaseResponse {
        error: false,
        message: "Forge webhook secret deleted.".to_string(),
    }))
}
