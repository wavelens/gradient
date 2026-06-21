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

mod response;
mod trigger;

pub use response::{QueuedEvaluation, SkippedProject, WebhookResponse, WebhookTriggerOutcome};

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_ci::{IntegrationKind, verify_github_signature};
use gradient_types::ForgeType;
use gradient_forge::WebhookEventKind;
use crate::ip_allowlist::is_allowed as ip_allowed;
use gradient_types::input::load_secret;
use gradient_types::*;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::client_ip::{OptionalPeer, resolve_client_ip};
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::ok_json;

use gradient_forge::{ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent, PushOutcome};
use trigger::{
    PushRefKind, handle_github_installation, resolve_github_app_targets,
    trigger_pr_for_integration, trigger_push_for_integration, trigger_release_for_integration,
};

// ── GitHub App webhook ─────────────────────────────────────────────────────

/// `POST /api/v1/hooks/github` - receives all events from the GitHub App.
pub async fn github_app_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    OptionalPeer(peer): OptionalPeer,
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

    let peer_ip = peer
        .map(|p| p.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let client_ip = resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    debug!(event = %event, "GitHub App webhook received");

    let response = match event.as_str() {
        "push" => match ParsedPushEvent::from_github(&body) {
            None => return Err(WebError::bad_request("malformed webhook payload")),
            Some(PushOutcome::Ignored) => WebhookResponse::empty("push"),
            Some(PushOutcome::Build(parsed)) => {
                let urls = parsed.repository_urls.clone();
                let outcome =
                    dispatch_github_app_push(&state, &scheduler, parsed, &body, client_ip).await;
                WebhookResponse {
                    event: "push".to_string(),
                    repository_urls: urls,
                    projects_scanned: outcome.projects_scanned,
                    queued: outcome.queued,
                    skipped: outcome.skipped,
                }
            }
        },
        "pull_request" => {
            let Some(parsed) = ParsedPullRequestEvent::from_github(&body) else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let outcome =
                dispatch_github_app_pr(&state, &scheduler, parsed, &body, client_ip).await;
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
            let outcome =
                dispatch_github_app_release(&state, &scheduler, parsed, &body, client_ip).await;
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
                client_ip,
            )
            .await;
            WebhookResponse::empty(&event)
        }
        "pull_request_review" => {
            trigger::handle_pull_request_review(&state, ForgeType::GitHub, None, &body, client_ip)
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
    client_ip: IpAddr,
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App push: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let targets =
        resolve_github_app_targets(state, installation_id, &parsed.repository_urls, client_ip)
            .await;
    if targets.is_empty() {
        warn!(
            installation_id,
            urls = ?parsed.repository_urls,
            "GitHub App push: no integration owns a project matching the webhook's repository"
        );
        return WebhookTriggerOutcome::default();
    }
    let ref_name = parsed.ref_name.clone();
    let is_tag = parsed.is_tag;
    let ref_kind = if is_tag {
        PushRefKind::Tag(&ref_name)
    } else {
        PushRefKind::Branch(&ref_name)
    };
    let mut combined = WebhookTriggerOutcome::default();
    for integration_id in targets {
        let outcome = trigger_push_for_integration(
            state,
            scheduler,
            integration_id,
            &parsed.repository_urls,
            ref_kind,
            parsed.commit_hash.clone(),
            parsed.commit_message.clone(),
            parsed.author_name.clone(),
        )
        .await;
        combined.projects_scanned += outcome.projects_scanned;
        combined.queued.extend(outcome.queued);
        combined.skipped.extend(outcome.skipped);
    }
    combined
}

async fn dispatch_github_app_pr(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedPullRequestEvent,
    body: &[u8],
    client_ip: IpAddr,
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App pull_request: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let targets =
        resolve_github_app_targets(state, installation_id, &parsed.repository_urls, client_ip)
            .await;
    if targets.is_empty() {
        warn!(
            installation_id,
            urls = ?parsed.repository_urls,
            "GitHub App pull_request: no integration owns a project matching the webhook's repository"
        );
        return WebhookTriggerOutcome::default();
    }
    let approval_ctx = approval_context_from(&parsed);
    let head_clone = parsed.head_repo_clone_url.clone();
    let mut combined = WebhookTriggerOutcome::default();
    for integration_id in targets {
        let outcome = trigger_pr_for_integration(
            state,
            scheduler,
            integration_id,
            &parsed.repository_urls,
            parsed.branch.as_deref(),
            &parsed.action,
            parsed.commit_hash.clone(),
            parsed.title.clone(),
            None,
            approval_ctx.clone(),
            head_clone.clone(),
            false,
            None,
            None,
        )
        .await;
        combined.projects_scanned += outcome.projects_scanned;
        combined.queued.extend(outcome.queued);
        combined.skipped.extend(outcome.skipped);
    }
    combined
}

fn approval_context_from(parsed: &ParsedPullRequestEvent) -> trigger::PullRequestApprovalContext {
    trigger::PullRequestApprovalContext {
        pr_number: parsed.pr_number,
        pr_author: parsed.pr_author.clone(),
        is_fork: parsed.is_fork,
        sender: parsed.sender.clone(),
    }
}

async fn dispatch_github_app_release(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedReleaseEvent,
    body: &[u8],
    client_ip: IpAddr,
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App release: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let targets =
        resolve_github_app_targets(state, installation_id, &parsed.repository_urls, client_ip)
            .await;
    if targets.is_empty() {
        warn!(
            installation_id,
            urls = ?parsed.repository_urls,
            "GitHub App release: no integration owns a project matching the webhook's repository"
        );
        return WebhookTriggerOutcome::default();
    }
    let mut combined = WebhookTriggerOutcome::default();
    for integration_id in targets {
        let outcome = trigger_release_for_integration(
            state,
            scheduler,
            integration_id,
            &parsed.repository_urls,
            parsed.tag.as_deref(),
            parsed.commit_hash.clone(),
            None,
            None,
        )
        .await;
        combined.projects_scanned += outcome.projects_scanned;
        combined.queued.extend(outcome.queued);
        combined.skipped.extend(outcome.skipped);
    }
    combined
}

// ── Generic forge webhook ──────────────────────────────────────────────────

/// `POST /api/v1/hooks/{forge}/{org_name}/{integration_name}` - receives push,
/// pull-request, and release events from a named inbound integration.
///
/// The `forge` path segment is one of: `gitea`, `forgejo`, `gitlab`.
pub async fn forge_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    OptionalPeer(peer): OptionalPeer,
    Path((forge, org_name, integration_name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<WebhookResponse>>> {
    let Some(forge_type) = ForgeType::from_path_segment(&forge) else {
        warn!(forge = %forge, "Unknown forge path segment");
        return Err(WebError::not_found_msg("integration not found"));
    };
    let Some(provider) = state.forge.get(forge_type).cloned() else {
        warn!(forge = %forge, "No forge provider registered");
        return Err(WebError::not_found_msg("integration not found"));
    };
    if !provider.accepts_per_integration_webhook() {
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

    let signature = first_header(&headers, &[provider.signature_header()]);
    if !provider.verify_signature(plaintext_secret.expose(), signature, &body) {
        warn!(org = %org_name, forge = %forge, integration = %integration_name, "Forge webhook: invalid signature");
        return Err(WebError::unauthorized("invalid webhook signature"));
    }

    let allowlist = integration.allowed_ips.clone().unwrap_or_default();
    if !allowlist.is_empty() {
        let peer_ip = peer
            .map(|p| p.ip())
            .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let client_ip =
            resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);
        if !ip_allowed(client_ip, &allowlist) {
            warn!(
                org = %org_name,
                forge = %forge,
                integration = %integration_name,
                %client_ip,
                "Forge webhook: source IP not allowed",
            );
            return Err(WebError::forbidden_with(
                ErrorCode::FORBIDDEN_SOURCE_IP,
                "Webhook source IP not allowed",
            ));
        }
    }

    let integration_id = integration.id;
    let raw_event = first_header(&headers, provider.event_headers());
    let event_type = provider.classify_event(raw_event);

    let response = match event_type {
        WebhookEventKind::Push => match provider.parse_push_event(&body) {
            None => return Err(WebError::bad_request("malformed webhook payload")),
            Some(PushOutcome::Ignored) => WebhookResponse::empty("push"),
            Some(PushOutcome::Build(parsed)) => {
                let urls = parsed.repository_urls.clone();
                let ref_name = parsed.ref_name.clone();
                let ref_kind = if parsed.is_tag {
                    PushRefKind::Tag(&ref_name)
                } else {
                    PushRefKind::Branch(&ref_name)
                };
                let outcome = trigger_push_for_integration(
                    &state,
                    &scheduler,
                    integration_id,
                    &urls,
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
        },
        WebhookEventKind::PullRequest => {
            let Some(parsed) = provider.parse_pull_request_event(&body) else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let approval_ctx = approval_context_from(&parsed);
            let head_clone = parsed.head_repo_clone_url.clone();
            let outcome = trigger_pr_for_integration(
                &state,
                &scheduler,
                integration_id,
                &urls,
                parsed.branch.as_deref(),
                &parsed.action,
                parsed.commit_hash.clone(),
                parsed.title.clone(),
                None,
                approval_ctx,
                head_clone,
                false,
                None,
                None,
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
        WebhookEventKind::Release => {
            let Some(parsed) = provider.parse_release_event(&body) else {
                return Err(WebError::bad_request("malformed webhook payload"));
            };
            let urls = parsed.repository_urls.clone();
            let outcome = trigger_release_for_integration(
                &state,
                &scheduler,
                integration_id,
                &urls,
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
        WebhookEventKind::Comment => {
            let peer_ip = peer
                .map(|p| p.ip())
                .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            let client_ip =
                resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);
            trigger::handle_issue_comment(
                &state,
                &scheduler,
                forge_type,
                Some(integration_id),
                &body,
                client_ip,
            )
            .await;
            WebhookResponse::empty("comment")
        }
        WebhookEventKind::Review => {
            let peer_ip = peer
                .map(|p| p.ip())
                .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            let client_ip =
                resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);
            trigger::handle_pull_request_review(
                &state,
                forge_type,
                Some(integration_id),
                &body,
                client_ip,
            )
            .await;
            WebhookResponse::empty("review")
        }
        WebhookEventKind::Unknown(name) => WebhookResponse::empty(&name),
    };

    Ok(ok_json(response))
}

/// First present header among `names`, as a `&str` (empty string if absent).
fn first_header<'a>(headers: &'a HeaderMap, names: &[&str]) -> &'a str {
    names
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
