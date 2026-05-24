/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Incoming forge webhook endpoints.
//!
//! These routes are **unauthenticated** - they verify callers via HMAC
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

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use gradient_core::ci::actions::decrypt_secret_with_file;
use gradient_core::ci::{
    ForgeType, IntegrationKind, verify_gitea_signature,
    verify_github_signature,
};
use gradient_core::types::input::load_secret;
use gradient_core::types::*;
use scheduler::Scheduler;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tracing::{debug, warn};

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;

use events::{ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent};
use trigger::{
    PushRefKind, handle_github_installation, resolve_github_integration_id,
    trigger_pr_for_integration, trigger_push_for_integration, trigger_release_for_integration,
};

// ── GitHub App webhook ─────────────────────────────────────────────────────

/// `POST /api/v1/hooks/github` - receives all events from the GitHub App.
pub async fn github_app_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let Some(github_app) = state.config.github_app.clone() else {
        warn!(
            "GitHub App webhook received but GitHub App is not fully configured \
             (requires GRADIENT_GITHUB_APP_ID, GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE, \
             and GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE)"
        );
        return Err(WebError::service_unavailable(
            "github app integration not configured",
        ));
    };
    let secret = match load_secret(&github_app.webhook_secret_file) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to load GitHub webhook secret");
            return Err(WebError::internal("webhook secret unavailable"));
        }
    };

    let sig_header = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !verify_github_signature(secret.expose(), sig_header, &body) {
        warn!("GitHub App webhook: invalid signature");
        return Err(WebError::unauthorized("invalid webhook signature"));
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
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let outcome = dispatch_github_app_push(&state, &scheduler, parsed, &body).await;
            WebhookResponse {
                event: "push".to_string(),
                repository_urls: urls,
                projects_scanned: outcome.projects_scanned,
                queued: outcome.queued,
                skipped: outcome.skipped,
            }
        }
        "pull_request" => {
            let Some(parsed) = ParsedPullRequestEvent::from_github(&body) else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let outcome = dispatch_github_app_pr(&state, &scheduler, parsed, &body).await;
            WebhookResponse {
                event: "pull_request".to_string(),
                repository_urls: urls,
                projects_scanned: outcome.projects_scanned,
                queued: outcome.queued,
                skipped: outcome.skipped,
            }
        }
        "release" => {
            let Some(parsed) = ParsedReleaseEvent::from_github(&body) else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let outcome = dispatch_github_app_release(&state, &scheduler, parsed, &body).await;
            WebhookResponse {
                event: "release".to_string(),
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
        "check_run" => {
            trigger::handle_github_check_run(&state, &scheduler, &body).await;
            WebhookResponse::empty(&event)
        }
        "issue_comment" => {
            trigger::handle_issue_comment(
                &state,
                &scheduler,
                ForgeType::GitHub,
                None,
                &body,
            )
            .await;
            WebhookResponse::empty(&event)
        }
        other => WebhookResponse::empty(other),
    };

    Ok(ok_json(response))
}

// ── GitHub App dispatch helpers ────────────────────────────────────────────

/// Extract installation_id from a GitHub App payload (push/pr/release all carry it).
fn github_installation_id_from_body(body: &[u8]) -> Option<i64> {
    #[derive(serde::Deserialize)]
    struct WithInstallation {
        installation: Option<InstallationId>,
    }
    #[derive(serde::Deserialize)]
    struct InstallationId {
        id: i64,
    }
    serde_json::from_slice::<WithInstallation>(body)
        .ok()
        .and_then(|p| p.installation)
        .map(|i| i.id)
}

async fn dispatch_github_app_push(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedPushEvent,
    body: &[u8],
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App push: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let Some(integration_id) = resolve_github_integration_id(state, installation_id).await else {
        warn!(
            installation_id,
            "GitHub App push: no integration found for installation"
        );
        return WebhookTriggerOutcome::default();
    };
    let ref_name = parsed.ref_name.clone();
    let is_tag = parsed.is_tag;
    let ref_kind = if is_tag {
        PushRefKind::Tag(&ref_name)
    } else {
        PushRefKind::Branch(&ref_name)
    };
    trigger_push_for_integration(
        state,
        scheduler,
        integration_id,
        ref_kind,
        parsed.commit_hash,
        parsed.commit_message,
        parsed.author_name,
    )
    .await
}

async fn dispatch_github_app_pr(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedPullRequestEvent,
    body: &[u8],
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App pull_request: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let Some(integration_id) = resolve_github_integration_id(state, installation_id).await else {
        warn!(
            installation_id,
            "GitHub App pull_request: no integration found for installation"
        );
        return WebhookTriggerOutcome::default();
    };
    let approval_ctx = approval_context_from(&parsed);
    trigger_pr_for_integration(
        state,
        scheduler,
        integration_id,
        parsed.branch.as_deref(),
        &parsed.action,
        parsed.commit_hash,
        None,
        None,
        approval_ctx,
    )
    .await
}

fn approval_context_from(parsed: &ParsedPullRequestEvent) -> trigger::PullRequestApprovalContext {
    trigger::PullRequestApprovalContext {
        pr_number: parsed.pr_number,
        pr_author: parsed.pr_author.clone(),
        is_fork: parsed.is_fork,
    }
}

async fn dispatch_github_app_release(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedReleaseEvent,
    body: &[u8],
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App release: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let Some(integration_id) = resolve_github_integration_id(state, installation_id).await else {
        warn!(
            installation_id,
            "GitHub App release: no integration found for installation"
        );
        return WebhookTriggerOutcome::default();
    };
    trigger_release_for_integration(
        state,
        scheduler,
        integration_id,
        parsed.tag.as_deref(),
        parsed.commit_hash,
        None,
        None,
    )
    .await
}

// ── Generic forge webhook ──────────────────────────────────────────────────

/// `POST /api/v1/hooks/{forge}/{org_name}/{integration_name}` - receives push,
/// pull-request, and release events from a named inbound integration.
///
/// The `forge` path segment is one of: `gitea`, `forgejo`, `gitlab`.
pub async fn forge_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((forge, org_name, integration_name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let Some(forge_type) = ForgeType::from_path_segment(&forge) else {
        warn!(forge = %forge, "Unknown forge path segment");
        return Err(WebError::not_found_msg("integration not found"));
    };
    if matches!(forge_type, ForgeType::GitHub) {
        return Err(WebError::bad_request("unsupported forge"));
    }

    let org = EOrganization::find()
        .filter(COrganization::Name.eq(org_name.as_str()))
        .one(&state.web_db)
        .await
        .map_err(|e| {
            warn!(error = %e, org = %org_name, "DB error looking up organization for forge webhook");
            WebError::internal("internal error")
        })?
        .ok_or_else(|| WebError::not_found_msg("integration not found"))?;

    let integration = EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Inbound)))
        .filter(CIntegration::Name.eq(integration_name.as_str()))
        .one(&state.web_db)
        .await
        .map_err(|e| {
            warn!(error = %e, "DB error looking up integration for forge webhook");
            WebError::internal("internal error")
        })?
        .ok_or_else(|| {
            warn!(org = %org_name, %forge, integration = %integration_name, "Integration not found");
            WebError::not_found_msg("integration not found")
        })?;

    let encrypted_secret = integration.secret.as_ref().ok_or_else(|| {
        warn!(integration_id = %integration.id, "Integration has no secret configured");
        WebError::not_found_msg("integration not found")
    })?;

    let plaintext_secret = decrypt_secret_with_file(
        &state.config.secrets.crypt_secret_file,
        encrypted_secret,
    )
    .map_err(|e| {
        warn!(error = %e, integration_id = %integration.id, "Failed to decrypt integration secret");
        WebError::internal("internal error")
    })?;

    if !verify_forge_signature(forge_type, plaintext_secret.expose(), &headers, &body) {
        warn!(org = %org_name, forge = %forge, integration = %integration_name, "Forge webhook: invalid signature");
        return Err(WebError::unauthorized("invalid webhook signature"));
    }

    let integration_id = integration.id;
    let event_type = forge_event_type(forge_type, &headers);

    let response = match event_type {
        ForgeEvent::Push => {
            let parsed = match forge_type {
                ForgeType::Gitea | ForgeType::Forgejo => ParsedPushEvent::from_gitea(&body),
                ForgeType::GitLab => ParsedPushEvent::from_gitlab(&body),
                ForgeType::GitHub => unreachable!(),
            };
            let Some(parsed) = parsed else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let ref_name = parsed.ref_name.clone();
            let is_tag = parsed.is_tag;
            let ref_kind = if is_tag {
                PushRefKind::Tag(&ref_name)
            } else {
                PushRefKind::Branch(&ref_name)
            };
            let outcome = trigger_push_for_integration(
                &state,
                &scheduler,
                integration_id,
                ref_kind,
                parsed.commit_hash,
                parsed.commit_message,
                parsed.author_name,
            )
            .await;
            WebhookResponse {
                event: "push".to_string(),
                repository_urls: urls,
                projects_scanned: outcome.projects_scanned,
                queued: outcome.queued,
                skipped: outcome.skipped,
            }
        }
        ForgeEvent::PullRequest => {
            let parsed = match forge_type {
                ForgeType::Gitea | ForgeType::Forgejo => ParsedPullRequestEvent::from_gitea(&body),
                ForgeType::GitLab => ParsedPullRequestEvent::from_gitlab(&body),
                ForgeType::GitHub => unreachable!(),
            };
            let Some(parsed) = parsed else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let approval_ctx = approval_context_from(&parsed);
            let outcome = trigger_pr_for_integration(
                &state,
                &scheduler,
                integration_id,
                parsed.branch.as_deref(),
                &parsed.action,
                parsed.commit_hash,
                None,
                None,
                approval_ctx,
            )
            .await;
            WebhookResponse {
                event: "pull_request".to_string(),
                repository_urls: urls,
                projects_scanned: outcome.projects_scanned,
                queued: outcome.queued,
                skipped: outcome.skipped,
            }
        }
        ForgeEvent::Release => {
            let parsed = match forge_type {
                ForgeType::Gitea | ForgeType::Forgejo => ParsedReleaseEvent::from_gitea(&body),
                ForgeType::GitLab => ParsedReleaseEvent::from_gitlab(&body),
                ForgeType::GitHub => unreachable!(),
            };
            let Some(parsed) = parsed else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let outcome = trigger_release_for_integration(
                &state,
                &scheduler,
                integration_id,
                parsed.tag.as_deref(),
                parsed.commit_hash,
                None,
                None,
            )
            .await;
            WebhookResponse {
                event: "release".to_string(),
                repository_urls: urls,
                projects_scanned: outcome.projects_scanned,
                queued: outcome.queued,
                skipped: outcome.skipped,
            }
        }
        ForgeEvent::Comment => {
            trigger::handle_issue_comment(
                &state,
                &scheduler,
                forge_type,
                Some(integration_id),
                &body,
            )
            .await;
            WebhookResponse::empty("comment")
        }
        ForgeEvent::Unknown(name) => WebhookResponse::empty(&name),
    };

    Ok(ok_json(response))
}

// ── Forge event type detection ─────────────────────────────────────────────

enum ForgeEvent {
    Push,
    PullRequest,
    Release,
    Comment,
    Unknown(String),
}

fn forge_event_type(forge: ForgeType, headers: &HeaderMap) -> ForgeEvent {
    match forge {
        ForgeType::Gitea | ForgeType::Forgejo => {
            let event = headers
                .get("X-Gitea-Event")
                .or_else(|| headers.get("X-Gogs-Event"))
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            match event {
                "push" => ForgeEvent::Push,
                "pull_request" => ForgeEvent::PullRequest,
                "release" => ForgeEvent::Release,
                "issue_comment" | "pull_request_comment" => ForgeEvent::Comment,
                other => ForgeEvent::Unknown(other.to_string()),
            }
        }
        ForgeType::GitLab => {
            let event = headers
                .get("X-Gitlab-Event")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            match event {
                "Push Hook" | "Tag Push Hook" => ForgeEvent::Push,
                "Merge Request Hook" => ForgeEvent::PullRequest,
                "Release Hook" => ForgeEvent::Release,
                "Note Hook" => ForgeEvent::Comment,
                other => ForgeEvent::Unknown(other.to_string()),
            }
        }
        ForgeType::GitHub => ForgeEvent::Unknown("github".into()),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

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
            token.as_bytes().ct_eq(secret.as_bytes()).into()
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
