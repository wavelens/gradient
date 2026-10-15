/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! These routes are unauthenticated. Callers are verified via HMAC signatures or token headers
//! instead of JWTs.

mod approval;
mod commands;
mod fanout;
mod installation;
mod payloads;
mod refusal;
mod response;

pub use response::{IncomingWebhookResponse, QueuedEvaluation, SkippedTask, WebhookTriggerOutcome};

use crate::ip_allowlist::is_allowed as ip_allowed;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use gradient_ci::CiContext;
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_ci::{IntegrationKind, verify_github_signature};
use gradient_core::ServerState;
use gradient_git_host::WebhookEventKind;
use gradient_scheduler::Scheduler;
use gradient_types::GitHostType;
use gradient_types::input::load_secret;
use gradient_types::*;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::client_ip::{OptionalPeer, resolve_client_ip};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use refusal::GitHostHookRefusal;

use fanout::{
    PushRefKind, trigger_pr_for_integration, trigger_push_for_integration,
    trigger_release_for_integration,
};
use gradient_git_host::{ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent, PushOutcome};
use installation::{handle_github_installation, resolve_github_app_targets};

#[utoipa::path(
    post,
    path = "/hooks/github",
    operation_id = "githubAppWebhook",
    tag = "git-host-webhooks",
    summary = "Receive GitHub App webhook",
    description = "Receives events from a configured GitHub App. Payloads are authenticated using\n`X-Hub-Signature-256` HMAC-SHA256 verification against the configured\n`GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`.\n\n**Handled events:**\n- `push` / `pull_request` / `release` - trigger an evaluation on any task whose\n  repository URL matches the matching trigger. A `push` carrying an\n  all-zero `after` SHA (a branch/tag deletion or a Git host \"test\" delivery)\n  is accepted as a `200` no-op rather than rejected.\n- `check_run` - the \"Approve and Run\" action clears a fork-PR approval gate.\n- `issue_comment` - `/gradient run` / `/gradient approve` / `/gradient summary` maintainer commands.\n- `pull_request_review` - a maintainer's approving review clears a fork-PR approval gate.\n- `installation` / `installation_repositories` - stores or clears the GitHub App\n  `installation_id` on the matching Gradient project.\n- `ping` - always returns 200.\n\nThis endpoint is unauthenticated (verified via HMAC signature).",
    params(("X-GitHub-Event" = String, Header), ("X-Hub-Signature-256" = String, Header)),
    request_body(content = serde_json::Value, content_type = "application/json"),
    responses((status = OK, description = "Webhook processed (may include zero queued evaluations)", body = IncomingWebhookResponse), WebError)
)]
pub async fn github_app_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<IncomingWebhookResponse>>> {
    let ci = state.ci();
    let Some(github_app) = state.config.github_app.clone() else {
        warn!(
            "GitHub App webhook received but GitHub App is not fully configured \
             (requires GRADIENT_GITHUB_APP_ID, GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE, \
             and GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE)"
        );
        return Err(GitHostHookRefusal::GithubAppNotConfigured.into());
    };
    let secret = match load_secret(&github_app.webhook_secret_file) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to load GitHub webhook secret");
            return Err(anyhow::anyhow!("webhook secret unavailable").into());
        }
    };

    let sig_header = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !verify_github_signature(secret.expose(), sig_header, &body) {
        warn!("GitHub App webhook: invalid signature");
        return Err(GitHostHookRefusal::InvalidSignature.into());
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
            None => return Err(GitHostHookRefusal::MalformedPayload.into()),
            Some(PushOutcome::Ignored) => IncomingWebhookResponse::empty("push"),
            Some(PushOutcome::Build(parsed)) => {
                let urls = parsed.repository_urls.clone();
                let outcome =
                    route_github_app_push(&ci, &scheduler, parsed, &body, client_ip).await;
                IncomingWebhookResponse::new("push", urls, outcome)
            }
        },
        "pull_request" => {
            let Some(parsed) = ParsedPullRequestEvent::from_github(&body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            let outcome = route_github_app_pr(&ci, &scheduler, parsed, &body, client_ip).await;
            IncomingWebhookResponse::new("pull_request", urls, outcome)
        }
        "release" => {
            let Some(parsed) = ParsedReleaseEvent::from_github(&body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            let outcome = route_github_app_release(&ci, &scheduler, parsed, &body, client_ip).await;
            IncomingWebhookResponse::new("release", urls, outcome)
        }
        "installation" | "installation_repositories" => {
            handle_github_installation(&state.web_db, &body).await;
            IncomingWebhookResponse::empty(&event)
        }
        "check_run" => {
            approval::handle_github_check_run(&ci, &scheduler, &body).await;
            IncomingWebhookResponse::empty(&event)
        }
        "issue_comment" => {
            commands::handle_issue_comment(
                &ci,
                &scheduler,
                GitHostType::GitHub,
                None,
                &body,
                client_ip,
            )
            .await;
            IncomingWebhookResponse::empty(&event)
        }
        "pull_request_review" => {
            approval::handle_pull_request_review(&ci, GitHostType::GitHub, None, &body, client_ip)
                .await;
            IncomingWebhookResponse::empty(&event)
        }
        other => IncomingWebhookResponse::empty(other),
    };

    Ok(ok_json(response))
}

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

async fn route_github_app_push(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedPushEvent,
    body: &[u8],
    client_ip: IpAddr,
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App push: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let targets = resolve_github_app_targets(
        &ci.db.web_db,
        installation_id,
        &parsed.repository_urls,
        client_ip,
    )
    .await;
    if targets.is_empty() {
        warn!(
            installation_id,
            urls = ?parsed.repository_urls,
            "GitHub App push: no integration owns a task matching the webhook's repository"
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
            ci,
            scheduler,
            integration_id,
            &parsed.repository_urls,
            ref_kind,
            parsed.commit_hash.clone(),
            parsed.commit_message.clone(),
            parsed.author_name.clone(),
        )
        .await;
        combined.tasks_scanned += outcome.tasks_scanned;
        combined.queued.extend(outcome.queued);
        combined.skipped.extend(outcome.skipped);
    }
    combined
}

async fn route_github_app_pr(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedPullRequestEvent,
    body: &[u8],
    client_ip: IpAddr,
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App pull_request: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let targets = resolve_github_app_targets(
        &ci.db.web_db,
        installation_id,
        &parsed.repository_urls,
        client_ip,
    )
    .await;
    if targets.is_empty() {
        warn!(
            installation_id,
            urls = ?parsed.repository_urls,
            "GitHub App pull_request: no integration owns a task matching the webhook's repository"
        );
        return WebhookTriggerOutcome::default();
    }
    let approval_ctx = approval::PullRequestApprovalContext::from(&parsed);
    let head_clone = parsed.head_repo_clone_url.clone();
    let mut combined = WebhookTriggerOutcome::default();
    for integration_id in targets {
        let outcome = trigger_pr_for_integration(
            ci,
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
        combined.tasks_scanned += outcome.tasks_scanned;
        combined.queued.extend(outcome.queued);
        combined.skipped.extend(outcome.skipped);
    }
    combined
}

async fn route_github_app_release(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    parsed: ParsedReleaseEvent,
    body: &[u8],
    client_ip: IpAddr,
) -> WebhookTriggerOutcome {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!("GitHub App release: missing installation_id");
        return WebhookTriggerOutcome::default();
    };
    let targets = resolve_github_app_targets(
        &ci.db.web_db,
        installation_id,
        &parsed.repository_urls,
        client_ip,
    )
    .await;
    if targets.is_empty() {
        warn!(
            installation_id,
            urls = ?parsed.repository_urls,
            "GitHub App release: no integration owns a task matching the webhook's repository"
        );
        return WebhookTriggerOutcome::default();
    }
    let mut combined = WebhookTriggerOutcome::default();
    for integration_id in targets {
        let outcome = trigger_release_for_integration(
            ci,
            scheduler,
            integration_id,
            &parsed.repository_urls,
            parsed.tag.as_deref(),
            parsed.commit_hash.clone(),
            None,
            None,
        )
        .await;
        combined.tasks_scanned += outcome.tasks_scanned;
        combined.queued.extend(outcome.queued);
        combined.skipped.extend(outcome.skipped);
    }
    combined
}

#[utoipa::path(
    post,
    path = "/hooks/{git_host}/{project}/{integration_name}",
    operation_id = "gitHostWebhook",
    tag = "git-host-webhooks",
    summary = "Receive Git host push webhook",
    description = "Receives push events for a named inbound integration.\nAuthenticated via the integration's stored HMAC secret.\n\nSignature header by Git host:\n- `gitea` / `forgejo`: `X-Gitea-Signature` (HMAC-SHA256 hex)\n- `gitlab`: `X-Gitlab-Token` (plain token comparison)\n\nOn a matching push, queues an evaluation for every active task whose\n`task_integration.inbound_integration` points at this row and whose\nrepository URL matches the webhook payload's clone URL.\n\nA push carrying an all-zero `after` SHA (a branch/tag deletion or a\nGit host \"test\" delivery) is accepted as a `200` no-op rather than rejected.\n\nThis endpoint is unauthenticated (verified via Git host specific signature).",
    params(("git_host" = String, Path), ("project" = String, Path), ("integration_name" = String, Path)),
    request_body(content = serde_json::Value, content_type = "application/json"),
    responses((status = OK, description = "Webhook processed (may include zero queued evaluations)", body = IncomingWebhookResponse), WebError)
)]
pub async fn git_host_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    OptionalPeer(peer): OptionalPeer,
    Path((git_host, project_name, integration_name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<IncomingWebhookResponse>>> {
    let ci = state.ci();
    let Some(git_host_type) = GitHostType::from_path_segment(&git_host) else {
        warn!(git_host = %git_host, "Unknown Git host path segment");
        return Err(GitHostHookRefusal::IntegrationNotFound.into());
    };
    let Some(provider) = state.git_host.get(git_host_type).cloned() else {
        warn!(git_host = %git_host, "No Git host provider registered");
        return Err(GitHostHookRefusal::IntegrationNotFound.into());
    };
    if !provider.accepts_per_integration_webhook() {
        return Err(GitHostHookRefusal::UnsupportedGitHost.into());
    }

    let project = gradient_db::projects::by_name(&state.web_db, project_name.as_str())
        .await
        .inspect_err(|e| {
            warn!(error = %e, project = %project_name, "DB error looking up project for Git host webhook");
        })
        .context("internal error")?
        .ok_or(GitHostHookRefusal::IntegrationNotFound)?;

    let integration = gradient_db::integrations::by_name(
        &state.web_db,
        project.id,
        IntegrationKind::Inbound,
        integration_name.as_str(),
    )
    .await
    .inspect_err(|e| {
        warn!(error = %e, "DB error looking up integration for Git host webhook");
    })
    .context("internal error")?
    .ok_or_else(|| {
        warn!(project = %project_name, git_host = %git_host, integration = %integration_name, "Integration not found");
        WebError::from(GitHostHookRefusal::IntegrationNotFound)
    })?;

    let encrypted_secret = integration.secret.as_ref().ok_or_else(|| {
        warn!(integration_id = %integration.id, "Integration has no secret configured");
        WebError::from(GitHostHookRefusal::IntegrationNotFound)
    })?;

    let plaintext_secret = decrypt_secret_with_file(
        &state.config.secrets.crypt_file,
        encrypted_secret,
    )
    .inspect_err(|e| {
        warn!(error = %e, integration_id = %integration.id, "Failed to decrypt integration secret");
    })
    .context("internal error")?;

    let signature = first_header(&headers, provider.signature_headers());
    if !provider.verify_signature(plaintext_secret.expose(), signature, &body) {
        warn!(project = %project_name, git_host = %git_host, integration = %integration_name, "Git host webhook: invalid signature");
        return Err(GitHostHookRefusal::InvalidSignature.into());
    }

    let allowlist = integration.allowed_ips.clone().unwrap_or_default();
    if !allowlist.is_empty() {
        let peer_ip = peer
            .map(|p| p.ip())
            .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let client_ip = resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);
        if !ip_allowed(client_ip, &allowlist) {
            warn!(
                project = %project_name,
                git_host = %git_host,
                integration = %integration_name,
                %client_ip,
                "Git host webhook: source IP not allowed",
            );
            return Err(GitHostHookRefusal::SourceIpNotAllowed.into());
        }
    }

    let integration_id = integration.id;
    let raw_event = first_header(&headers, provider.event_headers());
    let event_type = provider.classify_event(raw_event);

    let response = match event_type {
        WebhookEventKind::Push => match provider.parse_push_event(&body) {
            None => return Err(GitHostHookRefusal::MalformedPayload.into()),
            Some(PushOutcome::Ignored) => IncomingWebhookResponse::empty("push"),
            Some(PushOutcome::Build(parsed)) => {
                let urls = parsed.repository_urls.clone();
                let ref_name = parsed.ref_name.clone();
                let ref_kind = if parsed.is_tag {
                    PushRefKind::Tag(&ref_name)
                } else {
                    PushRefKind::Branch(&ref_name)
                };
                let outcome = trigger_push_for_integration(
                    &ci,
                    &scheduler,
                    integration_id,
                    &urls,
                    ref_kind,
                    parsed.commit_hash,
                    parsed.commit_message,
                    parsed.author_name,
                )
                .await;
                IncomingWebhookResponse::new("push", urls, outcome)
            }
        },
        WebhookEventKind::PullRequest => {
            let Some(parsed) = provider.parse_pull_request_event(&body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            let approval_ctx = approval::PullRequestApprovalContext::from(&parsed);
            let head_clone = parsed.head_repo_clone_url.clone();
            let outcome = trigger_pr_for_integration(
                &ci,
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
            IncomingWebhookResponse::new("pull_request", urls, outcome)
        }
        WebhookEventKind::Release => {
            let Some(parsed) = provider.parse_release_event(&body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            let outcome = trigger_release_for_integration(
                &ci,
                &scheduler,
                integration_id,
                &urls,
                parsed.tag.as_deref(),
                parsed.commit_hash,
                None,
                None,
            )
            .await;
            IncomingWebhookResponse::new("release", urls, outcome)
        }
        WebhookEventKind::Comment => {
            let peer_ip = peer
                .map(|p| p.ip())
                .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            let client_ip =
                resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);
            commands::handle_issue_comment(
                &ci,
                &scheduler,
                git_host_type,
                Some(integration_id),
                &body,
                client_ip,
            )
            .await;
            IncomingWebhookResponse::empty("comment")
        }
        WebhookEventKind::Review => {
            let peer_ip = peer
                .map(|p| p.ip())
                .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            let client_ip =
                resolve_client_ip(&headers, peer_ip, &state.config.network.trusted_proxies);
            approval::handle_pull_request_review(
                &ci,
                git_host_type,
                Some(integration_id),
                &body,
                client_ip,
            )
            .await;
            IncomingWebhookResponse::empty("review")
        }
        WebhookEventKind::Unknown(name) => IncomingWebhookResponse::empty(&name),
    };

    Ok(ok_json(response))
}

fn first_header<'a>(headers: &'a HeaderMap, names: &[&str]) -> &'a str {
    names
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
