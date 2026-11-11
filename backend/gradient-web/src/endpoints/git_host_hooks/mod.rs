/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! These routes are unauthenticated. Callers are verified via HMAC signatures or token headers
//! instead of JWTs.

mod approval;
mod commands;
mod delivery_log;
mod fanout;
mod flake_input_push;
pub(crate) mod installation;
mod payloads;
mod refusal;
mod response;

pub use response::{
    IncomingWebhookResponse, QueuedEvaluation, SkippedTask, TaskEvaluationNotStartedReason,
    WebhookTriggerOutcome,
};

use crate::ip_allowlist::is_allowed as ip_allowed;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use gradient_ci::CiContext;
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_ci::verify_github_signature;
use gradient_core::ServerState;
use gradient_db::git_host_connections::GitHostConnectionOwnerByDatabaseId;
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
use delivery_log::DeliveryLog;
use refusal::GitHostHookRefusal;

use fanout::{
    trigger_merge_group_for_connection, trigger_pr_for_connection, trigger_push_for_connection,
    trigger_release_for_connection,
};
use gradient_git_host::{
    MergeGroupOutcome, ParsedMergeGroupEvent, ParsedPullRequestEvent, ParsedPushEvent,
    ParsedReleaseEvent, PushOutcome,
};
pub(crate) use installation::repo_identity;
use installation::{handle_github_installation, resolve_github_app_targets};

#[utoipa::path(
    post,
    path = "/hooks/github",
    operation_id = "githubAppWebhook",
    tag = "git-host-webhooks",
    summary = "Receive GitHub App webhook",
    description = "Receives events from a configured GitHub App. Payloads are authenticated using\n`X-Hub-Signature-256` HMAC-SHA256 verification against the configured\n`GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE`.\n\n**Handled events:**\n- `push` / `pull_request` / `release` - trigger an evaluation on any task whose\n  repository URL matches the matching trigger. A `push` carrying an\n  all-zero `after` SHA (a branch/tag deletion or a Git host \"test\" delivery)\n  is accepted as a `200` no-op rather than rejected.\n- `merge_group` - `checks_requested` evaluates the merge group head on every push\n  trigger whose branch filter matches the queue's base branch.\n- `check_run` - the \"Approve and Run\" action clears a fork-PR approval gate.\n- `issue_comment` - `/gradient run` / `/gradient approve` / `/gradient summary` maintainer commands.\n- `pull_request_review` - a maintainer's approving review clears a fork-PR approval gate.\n- `installation` / `installation_repositories` - Create or update the connection to the Git host for the installation's instance. Delete that connection after installation removal.\n- `ping` - always returns 200.\n\nThis endpoint is unauthenticated (verified via HMAC signature).",
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
    let mut log = DeliveryLog::default();
    let result =
        handle_github_app_webhook(&state.ci(), &scheduler, peer, &headers, &body, &mut log).await;
    log.record(&state.web_db, &result).await;
    result.map(ok_json)
}

async fn handle_github_app_webhook(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    peer: Option<std::net::SocketAddr>,
    headers: &HeaderMap,
    body: &Bytes,
    log: &mut DeliveryLog,
) -> WebResult<IncomingWebhookResponse> {
    let Some(github_app) = ci.db.config.github_app.clone() else {
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

    if !verify_github_signature(secret.expose(), sig_header, body) {
        warn!("GitHub App webhook: invalid signature");
        return Err(GitHostHookRefusal::InvalidSignature.into());
    }

    let client_ip = webhook_sender_ip(ci, headers, peer);

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    debug!(event = %event, "GitHub App webhook received");

    log.event = Some(event.clone());
    let response = match event.as_str() {
        "push" => match ParsedPushEvent::from_github(body) {
            None => return Err(GitHostHookRefusal::MalformedPayload.into()),
            Some(PushOutcome::Ignored) => IncomingWebhookResponse::empty("push"),
            Some(PushOutcome::Build(parsed)) => {
                let urls = parsed.repository_urls.clone();
                log.connections = github_app_connections(ci, body, client_ip, "push").await;
                let outcome = route_github_app_push(ci, scheduler, &log.connections, parsed).await;
                IncomingWebhookResponse::new("push", urls, outcome)
            }
        },
        "pull_request" => {
            let Some(parsed) = ParsedPullRequestEvent::from_github(body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            log.connections = github_app_connections(ci, body, client_ip, "pull_request").await;
            let outcome = route_github_app_pr(ci, scheduler, &log.connections, parsed).await;
            IncomingWebhookResponse::new("pull_request", urls, outcome)
        }
        "merge_group" => match ParsedMergeGroupEvent::from_github(body) {
            None => return Err(GitHostHookRefusal::MalformedPayload.into()),
            Some(MergeGroupOutcome::Ignored) => IncomingWebhookResponse::empty("merge_group"),
            Some(MergeGroupOutcome::Build(parsed)) => {
                let urls = parsed.repository_urls.clone();
                log.connections = github_app_connections(ci, body, client_ip, "merge_group").await;
                let outcome =
                    route_github_app_merge_group(ci, scheduler, &log.connections, parsed).await;
                IncomingWebhookResponse::new("merge_group", urls, outcome)
            }
        },
        "release" => {
            let Some(parsed) = ParsedReleaseEvent::from_github(body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            log.connections = github_app_connections(ci, body, client_ip, "release").await;
            let outcome = route_github_app_release(ci, scheduler, &log.connections, parsed).await;
            IncomingWebhookResponse::new("release", urls, outcome)
        }
        "installation" | "installation_repositories" => {
            handle_github_installation(&ci.db.web_db, body).await;
            IncomingWebhookResponse::empty(&event)
        }
        "check_run" => {
            approval::handle_github_check_run(ci, body).await;
            IncomingWebhookResponse::empty(&event)
        }
        "issue_comment" => {
            commands::handle_issue_comment(
                ci,
                scheduler,
                GitHostType::GitHub,
                None,
                body,
                client_ip,
            )
            .await;
            IncomingWebhookResponse::empty(&event)
        }
        "pull_request_review" => {
            approval::handle_pull_request_review(ci, GitHostType::GitHub, None, body, client_ip)
                .await;
            IncomingWebhookResponse::empty(&event)
        }
        other => IncomingWebhookResponse::empty(other),
    };

    Ok(response)
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

async fn github_app_connections(
    ci: &CiContext,
    body: &[u8],
    client_ip: IpAddr,
    event: &str,
) -> Vec<GitHostConnectionId> {
    let Some(installation_id) = github_installation_id_from_body(body) else {
        warn!(event, "GitHub App webhook: missing installation_id");
        return Vec::new();
    };
    resolve_github_app_targets(&ci.db.web_db, installation_id, client_ip).await
}

async fn route_github_app_push(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    targets: &[GitHostConnectionId],
    parsed: ParsedPushEvent,
) -> WebhookTriggerOutcome {
    let mut combined = WebhookTriggerOutcome::default();
    for &connection_id in targets {
        combined.merge(trigger_push_for_connection(ci, scheduler, connection_id, &parsed).await);
    }

    combined
}

async fn route_github_app_merge_group(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    targets: &[GitHostConnectionId],
    parsed: ParsedMergeGroupEvent,
) -> WebhookTriggerOutcome {
    let mut combined = WebhookTriggerOutcome::default();
    for &connection_id in targets {
        combined
            .merge(trigger_merge_group_for_connection(ci, scheduler, connection_id, &parsed).await);
    }

    combined
}

async fn route_github_app_pr(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    targets: &[GitHostConnectionId],
    parsed: ParsedPullRequestEvent,
) -> WebhookTriggerOutcome {
    let approval_ctx = approval::PullRequestApprovalContext::from(&parsed);
    let mut combined = WebhookTriggerOutcome::default();
    for &connection_id in targets {
        combined.merge(
            trigger_pr_for_connection(
                ci,
                scheduler,
                connection_id,
                &parsed.repository_urls,
                parsed.branch.as_deref(),
                &parsed.action,
                parsed.commit_hash.clone(),
                parsed.title.clone(),
                None,
                approval_ctx.clone(),
                parsed.head_repo_clone_url.clone(),
                false,
                None,
                None,
            )
            .await,
        );
    }

    combined
}

async fn route_github_app_release(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    targets: &[GitHostConnectionId],
    parsed: ParsedReleaseEvent,
) -> WebhookTriggerOutcome {
    let mut combined = WebhookTriggerOutcome::default();
    for &connection_id in targets {
        combined.merge(
            trigger_release_for_connection(
                ci,
                scheduler,
                connection_id,
                &parsed.repository_urls,
                parsed.tag.as_deref(),
                &parsed.target,
                None,
                None,
            )
            .await,
        );
    }

    combined
}

#[utoipa::path(
    post,
    path = "/hooks/{git_host}/{project}/{connection_name}",
    operation_id = "gitHostWebhook",
    tag = "git-host-webhooks",
    summary = "Receive Git host push webhook",
    description = "Old webhook address can still serve requests. Gradient can find the connection for the project's Git host by name and handle requests like `POST /hooks/{connection}`. Path segment `git_host` can select the payload format.",
    params(("git_host" = String, Path), ("project" = String, Path), ("connection_name" = String, Path)),
    request_body(content = serde_json::Value, content_type = "application/json"),
    responses((status = OK, description = "Webhook processed (may include zero queued evaluations)", body = IncomingWebhookResponse), WebError)
)]
pub async fn git_host_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    OptionalPeer(peer): OptionalPeer,
    Path(target): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<IncomingWebhookResponse>>> {
    let mut log = DeliveryLog::default();
    let result = handle_git_host_webhook(
        &state.ci(),
        &scheduler,
        peer,
        target,
        &headers,
        &body,
        &mut log,
    )
    .await;
    log.record(&state.web_db, &result).await;
    result.map(ok_json)
}

#[utoipa::path(
    post,
    path = "/hooks/{connection}",
    operation_id = "gitHostConnectionWebhook",
    tag = "git-host-webhooks",
    summary = "Receive Git host webhook",
    description = "Accept events from a Git host for one Git host connection. Authenticate the request with the secret stored on the connection.\n\nSignature headers for each Git host.\n- `gitea` / `forgejo`: `X-Gitea-Signature` (HMAC-SHA256 as hex)\n- `gitlab`: `X-Gitlab-Token` (a comparison of a plaintext token)\n\nPushes queue evaluations for all active tasks that use this connection, have a matching trigger, and have a repository URL equal to the clone URL in the payload. Pushes with an all-zero `after` SHA (deleted branch or tag, or a test delivery) return `200` and do nothing.\n\nRequests to this endpoint need no login. Signatures prove who the sender is.",
    params(("connection" = GitHostConnectionId, Path)),
    request_body(content = serde_json::Value, content_type = "application/json"),
    responses((status = OK, description = "Webhook processed (may include zero queued evaluations)", body = IncomingWebhookResponse), WebError)
)]
pub async fn git_host_connection_webhook(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    OptionalPeer(peer): OptionalPeer,
    Path(connection): Path<GitHostConnectionId>,
    headers: HeaderMap,
    body: Bytes,
) -> WebResult<Json<BaseResponse<IncomingWebhookResponse>>> {
    let ci = state.ci();
    let mut log = DeliveryLog::default();
    let result =
        match gradient_db::lookup::by_id::<EGitHostConnection, _>(&ci.db.web_db, connection)
            .await
            .context("internal error")
        {
            Ok(Some(connection)) if connection.github_installation_id.is_none() => {
                let git_host_type = connection.git_host_type;
                verify_webhook_signature_and_ip_allowlist_then_trigger_event_evaluations(
                    &ci,
                    &scheduler,
                    peer,
                    git_host_type,
                    connection,
                    &headers,
                    &body,
                    &mut log,
                )
                .await
            }
            Ok(_) => Err(GitHostHookRefusal::GitHostConnectionNotFoundOrNotReceivingEvents.into()),
            Err(e) => Err(e.into()),
        };
    log.record(&state.web_db, &result).await;
    result.map(ok_json)
}

async fn handle_git_host_webhook(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    peer: Option<std::net::SocketAddr>,
    (git_host, project_name, connection_name): (String, String, String),
    headers: &HeaderMap,
    body: &Bytes,
    log: &mut DeliveryLog,
) -> WebResult<IncomingWebhookResponse> {
    let Some(git_host_type) = GitHostType::from_path_segment(&git_host) else {
        warn!(git_host = %git_host, "Unknown Git host path segment");
        return Err(GitHostHookRefusal::GitHostConnectionNotFoundOrNotReceivingEvents.into());
    };
    let project = gradient_db::projects::by_name(&ci.db.web_db, project_name.as_str())
        .await
        .context("internal error")?
        .ok_or(GitHostHookRefusal::GitHostConnectionNotFoundOrNotReceivingEvents)?;
    let connection = gradient_db::git_host_connections::by_name(
        &ci.db.web_db,
        GitHostConnectionOwnerByDatabaseId::Project(project.id),
        connection_name.as_str(),
    )
    .await
    .context("internal error")?
    .ok_or(GitHostHookRefusal::GitHostConnectionNotFoundOrNotReceivingEvents)?;
    verify_webhook_signature_and_ip_allowlist_then_trigger_event_evaluations(
        ci,
        scheduler,
        peer,
        git_host_type,
        connection,
        headers,
        body,
        log,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "Too many arguments. #503 tracks the refactor."
)]
async fn verify_webhook_signature_and_ip_allowlist_then_trigger_event_evaluations(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    peer: Option<std::net::SocketAddr>,
    git_host_type: GitHostType,
    connection: MGitHostConnection,
    headers: &HeaderMap,
    body: &Bytes,
    log: &mut DeliveryLog,
) -> WebResult<IncomingWebhookResponse> {
    if !connection.receives_events {
        return Err(GitHostHookRefusal::GitHostConnectionNotFoundOrNotReceivingEvents.into());
    }
    let Some(provider) = ci.git_host.get(git_host_type).cloned() else {
        warn!(
            git_host = git_host_type.as_path_segment(),
            "No Git host provider registered"
        );
        return Err(GitHostHookRefusal::GitHostConnectionNotFoundOrNotReceivingEvents.into());
    };
    if !provider.accepts_per_integration_webhook() {
        return Err(GitHostHookRefusal::UnsupportedGitHost.into());
    }
    let raw_event = first_header(headers, provider.event_headers());

    let Some(encrypted_secret) = connection.secret.as_ref() else {
        warn!(connection = %connection.id, "Git host connection has no webhook secret");
        log.set_connection_and_event(connection.id, raw_event);
        return Err(GitHostHookRefusal::SecretNotConfigured.into());
    };

    let plaintext_secret = decrypt_secret_with_file(
        &ci.db.config.secrets.crypt_file,
        encrypted_secret,
    )
    .inspect_err(|e| {
        warn!(error = %e, connection = %connection.id, "Failed to decrypt the webhook secret of the Git host connection");
    })
    .context("internal error")?;

    let signature = first_header(headers, provider.signature_headers());
    if !provider.verify_signature(plaintext_secret.expose(), signature, body) {
        warn!(connection = %connection.id, "Git host webhook: invalid signature");
        return Err(GitHostHookRefusal::InvalidSignature.into());
    }
    log.set_connection_and_event(connection.id, raw_event);

    let client_ip = webhook_sender_ip(ci, headers, peer);
    let allowlist = connection.allowed_ips.clone().unwrap_or_default();
    if !allowlist.is_empty() && !ip_allowed(client_ip, &allowlist) {
        warn!(connection = %connection.id, %client_ip, "Git host webhook: source IP not allowed");
        return Err(GitHostHookRefusal::SourceIpNotAllowed.into());
    }

    let connection_id = connection.id;
    let event_type = provider.classify_event(raw_event);

    let response = match event_type {
        WebhookEventKind::Push => match provider.parse_push_event(body) {
            None => return Err(GitHostHookRefusal::MalformedPayload.into()),
            Some(PushOutcome::Ignored) => IncomingWebhookResponse::empty("push"),
            Some(PushOutcome::Build(parsed)) => {
                let outcome =
                    trigger_push_for_connection(ci, scheduler, connection_id, &parsed).await;
                IncomingWebhookResponse::new("push", parsed.repository_urls, outcome)
            }
        },
        WebhookEventKind::PullRequest => {
            let Some(parsed) = provider.parse_pull_request_event(body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            let approval_ctx = approval::PullRequestApprovalContext::from(&parsed);
            let head_clone = parsed.head_repo_clone_url.clone();
            let outcome = trigger_pr_for_connection(
                ci,
                scheduler,
                connection_id,
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
            let Some(parsed) = provider.parse_release_event(body) else {
                return Err(GitHostHookRefusal::MalformedPayload.into());
            };
            let urls = parsed.repository_urls.clone();
            let outcome = trigger_release_for_connection(
                ci,
                scheduler,
                connection_id,
                &urls,
                parsed.tag.as_deref(),
                &parsed.target,
                None,
                None,
            )
            .await;
            IncomingWebhookResponse::new("release", urls, outcome)
        }
        WebhookEventKind::Comment => {
            commands::handle_issue_comment(
                ci,
                scheduler,
                git_host_type,
                Some(connection_id),
                body,
                client_ip,
            )
            .await;
            IncomingWebhookResponse::empty("comment")
        }
        WebhookEventKind::Review => {
            approval::handle_pull_request_review(
                ci,
                git_host_type,
                Some(connection_id),
                body,
                client_ip,
            )
            .await;
            IncomingWebhookResponse::empty("review")
        }
        WebhookEventKind::Unknown(name) => IncomingWebhookResponse::empty(&name),
    };

    Ok(response)
}

fn first_header<'a>(headers: &'a HeaderMap, names: &[&str]) -> &'a str {
    names
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

fn webhook_sender_ip(
    ci: &CiContext,
    headers: &HeaderMap,
    peer: Option<std::net::SocketAddr>,
) -> IpAddr {
    let peer_ip = peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |p| p.ip());
    resolve_client_ip(headers, peer_ip, &ci.db.config.network.trusted_proxies)
}
