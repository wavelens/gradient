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

use crate::error::{WebError, WebResult};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use core::ci::decrypt_webhook_secret;
use core::ci::{TriggerError, trigger_evaluation};
use core::ci::{verify_gitea_signature, verify_github_signature};
use core::types::input::load_secret;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ── GitHub App webhook ─────────────────────────────────────────────────────

/// Payload fields we care about for a GitHub `push` event.
#[derive(Deserialize)]
struct GitHubPushPayload {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String, // head commit SHA (hex)
    repository: GitHubRepository,
}

#[derive(Deserialize)]
struct GitHubRepository {
    clone_url: String,
    ssh_url: String,
}

/// Payload for `installation` / `installation_repositories` events.
#[derive(Deserialize)]
struct GitHubInstallationPayload {
    action: String,
    installation: GitHubInstallation,
    /// Present on `installation` events; tells us which GitHub account owns it.
    sender: Option<GitHubSender>,
}

#[derive(Deserialize)]
struct GitHubInstallation {
    id: i64,
    account: GitHubAccount,
}

#[derive(Deserialize)]
struct GitHubAccount {
    login: String,
}

#[derive(Deserialize)]
struct GitHubSender {
    login: String,
}

/// `POST /api/v1/hooks/github` — receives all events from the GitHub App.
pub async fn github_app_webhook(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // 1. Verify HMAC signature.
    let Some(ref secret_file) = state.cli.github_app_webhook_secret_file else {
        warn!(
            "GitHub App webhook received but GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE is not configured"
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let secret = load_secret(secret_file);

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
            handle_github_push(&state, &body).await;
            StatusCode::OK
        }
        "installation" | "installation_repositories" => {
            handle_github_installation(&state, &body).await;
            StatusCode::OK
        }
        _ => StatusCode::OK,
    }
}

async fn handle_github_push(state: &Arc<ServerState>, body: &[u8]) {
    let payload: GitHubPushPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse GitHub push payload");
            return;
        }
    };

    // Skip tag pushes and deletions.
    if !payload.git_ref.starts_with("refs/heads/")
        || payload.after == "0000000000000000000000000000000000000000"
    {
        return;
    }

    let commit_bytes = match hex::decode(&payload.after) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, sha = %payload.after, "GitHub push: invalid commit SHA");
            return;
        }
    };

    // Find projects matching either the HTTPS clone URL or the SSH URL.
    let candidate_urls = [
        payload.repository.clone_url.as_str(),
        payload.repository.ssh_url.as_str(),
    ];

    trigger_for_repo_urls(state, &candidate_urls, commit_bytes, None, None).await;
}

async fn handle_github_installation(state: &Arc<ServerState>, body: &[u8]) {
    let payload: GitHubInstallationPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse GitHub installation payload");
            return;
        }
    };

    if payload.action == "deleted" {
        // Clear installation_id on all orgs that had this installation.
        if let Ok(orgs) = EOrganization::find()
            .filter(COrganization::GithubInstallationId.eq(payload.installation.id))
            .all(&state.db)
            .await
        {
            for org in orgs {
                let mut active = org.into_active_model();
                active.github_installation_id = Set(None);
                if let Err(e) = active.update(&state.db).await {
                    warn!(error = %e, "Failed to clear github_installation_id");
                }
            }
        }
        return;
    }

    // For created/new_permissions_accepted: find the Gradient org whose name
    // matches the GitHub account login.
    let github_login = &payload.installation.account.login;
    if let Ok(Some(org)) = EOrganization::find()
        .filter(COrganization::Name.eq(github_login.as_str()))
        .one(&state.db)
        .await
    {
        let installation_id = payload.installation.id;
        let mut active = org.into_active_model();
        active.github_installation_id = Set(Some(installation_id));
        if let Err(e) = active.update(&state.db).await {
            warn!(error = %e, installation_id, org_name = %github_login, "Failed to store github_installation_id");
        } else {
            info!(installation_id, org_name = %github_login, "GitHub App installed on organization");
        }
    } else {
        let sender_login = payload
            .sender
            .as_ref()
            .map(|s| s.login.as_str())
            .unwrap_or("unknown");
        warn!(
            github_login = %github_login,
            sender = %sender_login,
            installation_id = payload.installation.id,
            "GitHub App installed but no matching Gradient organization found"
        );
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

    // Verify signature — method varies by forge.
    let verified = match forge.as_str() {
        "gitea" | "forgejo" => {
            let sig = headers
                .get("X-Gitea-Signature")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            verify_gitea_signature(plaintext_secret.expose(), sig, &body)
        }
        "gitlab" => {
            // GitLab sends a plain token in X-Gitlab-Token; no HMAC.
            let token = headers
                .get("X-Gitlab-Token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            token == plaintext_secret.expose()
        }
        "github" => {
            // GitHub without the App: same HMAC as App webhooks.
            let sig = headers
                .get("X-Hub-Signature-256")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            verify_github_signature(plaintext_secret.expose(), sig, &body)
        }
        unknown => {
            warn!(forge = %unknown, "Unknown forge type in webhook path");
            return StatusCode::BAD_REQUEST;
        }
    };

    if !verified {
        warn!(org = %org_name, forge = %forge, "Forge webhook: invalid signature");
        return StatusCode::UNAUTHORIZED;
    }

    // Parse push payload.  All major forges use a similar shape.
    match forge.as_str() {
        "gitea" | "forgejo" | "github" => handle_gitea_push(&state, &body).await,
        "gitlab" => handle_gitlab_push(&state, &body).await,
        _ => {}
    }

    StatusCode::OK
}

// ── Gitea/Forgejo push payload ─────────────────────────────────────────────

#[derive(Deserialize)]
struct GiteaPushPayload {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String,
    repository: GiteaRepository,
}

#[derive(Deserialize)]
struct GiteaRepository {
    clone_url: String,
    ssh_url: Option<String>,
}

async fn handle_gitea_push(state: &Arc<ServerState>, body: &[u8]) {
    let payload: GiteaPushPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse Gitea push payload");
            return;
        }
    };

    if !payload.git_ref.starts_with("refs/heads/")
        || payload.after == "0000000000000000000000000000000000000000"
    {
        return;
    }

    let commit_bytes = match hex::decode(&payload.after) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, sha = %payload.after, "Gitea push: invalid commit SHA");
            return;
        }
    };

    let mut urls = vec![payload.repository.clone_url.as_str()];
    if let Some(ref ssh) = payload.repository.ssh_url {
        urls.push(ssh.as_str());
    }

    trigger_for_repo_urls(state, &urls, commit_bytes, None, None).await;
}

// ── GitLab push payload ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GitLabPushPayload {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String,
    project: GitLabProject,
}

#[derive(Deserialize)]
struct GitLabProject {
    http_url: String,
    ssh_url: Option<String>,
}

async fn handle_gitlab_push(state: &Arc<ServerState>, body: &[u8]) {
    let payload: GitLabPushPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse GitLab push payload");
            return;
        }
    };

    if !payload.git_ref.starts_with("refs/heads/")
        || payload.after == "0000000000000000000000000000000000000000"
    {
        return;
    }

    let commit_bytes = match hex::decode(&payload.after) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, sha = %payload.after, "GitLab push: invalid commit SHA");
            return;
        }
    };

    let mut urls = vec![payload.project.http_url.as_str()];
    if let Some(ref ssh) = payload.project.ssh_url {
        urls.push(ssh.as_str());
    }

    trigger_for_repo_urls(state, &urls, commit_bytes, None, None).await;
}

// ── Shared helper ──────────────────────────────────────────────────────────

/// Finds all active projects whose repository URL matches any of `candidate_urls`
/// and queues an evaluation for each one.
async fn trigger_for_repo_urls(
    state: &Arc<ServerState>,
    candidate_urls: &[&str],
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) {
    // Normalise URLs for comparison: strip trailing `.git`.
    let normalise = |u: &str| u.trim_end_matches(".git").to_string();
    let normalised_candidates: Vec<String> = candidate_urls.iter().map(|u| normalise(u)).collect();

    let projects = match EProject::find()
        .filter(CProject::Active.eq(true))
        .all(&state.db)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "DB error fetching projects for forge webhook");
            return;
        }
    };

    for project in projects {
        let repo_normalised = normalise(&project.repository);
        let matches = normalised_candidates.iter().any(|c| c == &repo_normalised);

        if !matches {
            continue;
        }

        match trigger_evaluation(
            &state.db,
            &project,
            commit_hash.clone(),
            commit_message.clone(),
            author_name.clone(),
        )
        .await
        {
            Ok(eval) => {
                info!(
                    project_id = %project.id,
                    evaluation_id = %eval.id,
                    "Forge webhook triggered evaluation"
                );
            }
            Err(TriggerError::AlreadyInProgress) => {
                debug!(project_id = %project.id, "Evaluation already in progress, skipping webhook trigger");
            }
            Err(TriggerError::NoPreviousEvaluation) => {}
            Err(TriggerError::Db(e)) => {
                warn!(error = %e, project_id = %project.id, "DB error triggering evaluation from forge webhook");
            }
        }
    }
}

// ── Org forge webhook secret management ───────────────────────────────────

/// Response for the forge webhook secret endpoint.
#[derive(serde::Serialize)]
pub struct ForgeWebhookSecretResponse {
    pub webhook_url: String,
    pub secret: String,
}

/// `POST /api/v1/orgs/{organization}/forge-webhook-secret` — generates or
/// rotates the per-org forge webhook secret. Returns the plaintext secret
/// **once** (it is stored encrypted and never exposed again).
pub async fn post_forge_webhook_secret(
    State(state): State<Arc<ServerState>>,
    axum::Extension(user): axum::Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<axum::Json<BaseResponse<ForgeWebhookSecretResponse>>> {
    use super::projects::user_can_edit;
    use core::ci::encrypt_webhook_secret;
    use rand::RngExt;

    let org = EOrganization::find()
        .filter(COrganization::Name.eq(organization.as_str()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    if !user_can_edit(&state, user.id, org.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to manage forge webhooks for this organization."
                .to_string(),
        ));
    }

    // Generate a 32-byte random secret, hex-encoded.
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
/// per-org forge webhook secret, disabling inbound forge webhook verification.
pub async fn delete_forge_webhook_secret(
    State(state): State<Arc<ServerState>>,
    axum::Extension(user): axum::Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<axum::Json<BaseResponse<String>>> {
    use super::projects::user_can_edit;

    let org = EOrganization::find()
        .filter(COrganization::Name.eq(organization.as_str()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    if !user_can_edit(&state, user.id, org.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to manage forge webhooks for this organization."
                .to_string(),
        ));
    }

    let mut active = org.into_active_model();
    active.forge_webhook_secret = Set(None);
    active.update(&state.db).await?;

    Ok(axum::Json(BaseResponse {
        error: false,
        message: "Forge webhook secret deleted.".to_string(),
    }))
}
