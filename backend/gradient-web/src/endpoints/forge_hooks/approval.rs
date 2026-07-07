/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Approval-gate unparking: GitHub check-run buttons and native PR reviews.

use super::commands::{
    active_project_ids_for_integration, first_project_with_reporter,
    github_installation_id_from_comment_body,
};
use super::installation::resolve_github_app_targets;
use super::payloads::GithubCheckRunRequestedAction;
use gradient_ci::{APPROVAL_ACTION_ID, find_approval_gated_eval, unpark_approval};
use gradient_core::ServerState;
use gradient_forge::ParsedPullRequestReviewEvent;
use gradient_scheduler::Scheduler;
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::{info, warn};

/// PR metadata extracted from a forge webhook, used by the approval gate. A
/// trusted `sender` (maintainer force-push / command) bypasses the gate so the
/// run is not re-parked.
#[derive(Debug, Clone, Default)]
pub(super) struct PullRequestApprovalContext {
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub is_fork: Option<bool>,
    pub sender: Option<String>,
}

/// Handle a GitHub App `check_run.requested_action` event. Verifies the sender
/// is a repo writer, then re-queues the approval-gated evaluation matched by
/// `check_run.id` and fires a fresh pending check.
pub(super) async fn handle_github_check_run(
    state: &Arc<ServerState>,
    _scheduler: &Arc<Scheduler>,
    body: &[u8],
) {
    let payload: GithubCheckRunRequestedAction = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "GitHub check_run: failed to parse payload");
            return;
        }
    };
    if payload.action != "requested_action" {
        return;
    }
    let Some(action) = payload.requested_action else {
        return;
    };
    if action.identifier != APPROVAL_ACTION_ID {
        return;
    }
    let Some(sender) = payload.sender else {
        warn!("GitHub check_run.requested_action: missing sender");
        return;
    };
    let Some(full_name) = payload.repository.full_name else {
        warn!("GitHub check_run.requested_action: missing repository.full_name");
        return;
    };
    let Some((owner, repo)) = full_name.split_once('/') else {
        warn!(
            full_name,
            "GitHub check_run.requested_action: malformed repo full_name"
        );
        return;
    };

    let check_id = payload.check_run.id;
    let Some(eval) = find_eval_by_check_id(state, check_id).await else {
        warn!(
            check_run_id = check_id,
            "GitHub check_run.requested_action: no evaluation with matching repo_check_id"
        );
        return;
    };
    let Some(project_id) = eval.project else {
        return;
    };

    if !sender_is_trusted(state, project_id, owner, repo, sender.login).await {
        warn!(
            evaluation_id = %eval.id,
            sender = %sender.login,
            "Rejecting approval click - sender is not a repo writer"
        );
        return;
    }

    match unpark_approval(&state.web_db, eval.id).await {
        Ok(Some(unparked)) => {
            info!(
                evaluation_id = %eval.id,
                sender = %sender.login,
                "PR approval gate cleared via GitHub action"
            );
            dispatch_approval_granted(state, &unparked).await;
            if let Some(pr_number) = approval_pr_number(&eval) {
                submit_pr_approval_review(state, project_id, owner, repo, pr_number).await;
            }
        }
        Ok(None) => {
            warn!(
                evaluation_id = %eval.id,
                "Approval click but evaluation no longer in Waiting+Approval state"
            );
        }
        Err(e) => warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate"),
    }
}

/// Flip the `Awaiting Approval` check to Success once the gate is cleared, and
/// re-emit the Evaluation check as Pending so the PR shows the run in flight.
pub(super) async fn dispatch_approval_granted(state: &Arc<ServerState>, eval: &MEvaluation) {
    let Some(project_id) = eval.project else {
        return;
    };
    let payload = serde_json::json!({
        "evaluation_id": eval.id,
        "project_id": project_id,
        "status": "evaluation.approval_granted",
    });
    gradient_ci::actions::dispatch_evaluation_event(
        &state.ci(),
        project_id,
        "evaluation.approval_granted",
        payload,
    )
    .await;

    gradient_ci::actions::dispatch_evaluation_created(&state.ci(), eval).await;
}

async fn find_eval_by_check_id(state: &Arc<ServerState>, check_id: i64) -> Option<MEvaluation> {
    // `evaluation.check_run_ids` is a JSON map keyed by check-context name; any
    // stored id can match the clicked check, so scan the map's values.
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    #[derive(FromQueryResult)]
    struct Row {
        id: uuid::Uuid,
    }

    let row = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT id FROM evaluation
           WHERE check_run_ids IS NOT NULL
             AND EXISTS (
                 SELECT 1
                 FROM jsonb_each(check_run_ids) AS kv
                 WHERE (kv.value)::text::bigint = $1
             )
           LIMIT 1"#,
        [sea_orm::Value::BigInt(Some(check_id))],
    ))
    .one(&state.web_db)
    .await
    .ok()
    .flatten()?;

    EEvaluation::find_by_id(gradient_entity::ids::EvaluationId::new(row.id))
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
}

/// Trust probe for the approval-unpark flows: asks the project's reporter
/// whether `sender` can write to `owner/repo`. Fails closed on any error.
pub(super) async fn sender_is_trusted(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    owner: &str,
    repo: &str,
    sender: &str,
) -> bool {
    let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return false,
        Err(e) => {
            warn!(error = %e, %project_id, "resolving ForgeStatusReport action for trust probe");
            return false;
        }
    };
    match reporter.is_repo_writer(owner, repo, sender).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, %project_id, "is_repo_writer probe failed");
            false
        }
    }
}

/// Find the approval-gated evaluation for `(project_id, pr_number)`, flip it
/// back to `Queued`, and re-emit its pending CI checks.
async fn unpark_pr_approval_eval(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    pr_number: u64,
) -> Option<MEvaluation> {
    let eval = find_approval_gated_eval(&state.web_db, project_id, pr_number)
        .await
        .ok()
        .flatten()?;
    match unpark_approval(&state.web_db, eval.id).await {
        Ok(Some(unparked)) => {
            dispatch_approval_granted(state, &unparked).await;
            Some(unparked)
        }
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate via review");
            None
        }
    }
}

fn approval_pr_number(eval: &MEvaluation) -> Option<u64> {
    match eval
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)?
    {
        WaitingReason::Approval { pr_number, .. } => Some(pr_number),
        _ => None,
    }
}

/// Reflect a Gradient maintainer approval back onto the forge by submitting an
/// approving PR review (GitHub only; other forges no-op). Best-effort.
pub(super) async fn submit_pr_approval_review(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    owner: &str,
    repo: &str,
    pr_number: u64,
) {
    let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %project_id, "resolving reporter for PR approval review");
            return;
        }
    };

    if let Err(e) = reporter
        .approve_pull_request(
            owner,
            repo,
            pr_number,
            "Approved via Gradient maintainer approval gate.",
        )
        .await
    {
        warn!(error = %e, %project_id, pr_number, "submitting forge PR approval review failed");
    }
}

/// Handle a `pull_request_review` webhook: a maintainer's native approving
/// review releases an approval-gated run for the PR (#369). GitLab is a no-op.
pub(super) async fn handle_pull_request_review(
    state: &Arc<ServerState>,
    forge: ForgeType,
    integration_id: Option<IntegrationId>,
    body: &[u8],
    client_ip: std::net::IpAddr,
) {
    let parsed = match forge {
        ForgeType::GitHub => ParsedPullRequestReviewEvent::from_github(body),
        ForgeType::Gitea | ForgeType::Forgejo => ParsedPullRequestReviewEvent::from_gitea(body),
        ForgeType::GitLab => return,
    };
    let Some(review) = parsed else {
        return;
    };
    if !review.approved {
        return;
    }

    let Some(pr_number) = review.pr_number else {
        warn!("pull_request_review: approval without a PR number");
        return;
    };
    let Some(reviewer) = review.reviewer else {
        warn!(
            pr_number,
            "pull_request_review: approval without a reviewer"
        );
        return;
    };
    let Some(owner_repo) = review.repository_full_name else {
        warn!(
            pr_number,
            "pull_request_review: approval without a repository"
        );
        return;
    };
    let Some((owner, repo)) = owner_repo.rsplit_once('/') else {
        warn!(owner_repo, "pull_request_review: malformed repo full_name");
        return;
    };

    let integration_ids: Vec<IntegrationId> = match integration_id {
        Some(id) => vec![id],
        None => {
            let Some(installation_id) = github_installation_id_from_comment_body(body) else {
                warn!("pull_request_review (github): no installation_id");
                return;
            };
            let repo_urls = vec![
                format!("https://github.com/{owner_repo}"),
                format!("https://github.com/{owner_repo}.git"),
                format!("git@github.com:{owner_repo}.git"),
            ];
            let targets =
                resolve_github_app_targets(state, installation_id, &repo_urls, client_ip).await;
            if targets.is_empty() {
                warn!(installation_id, %owner_repo, "pull_request_review (github): no integration matched");
                return;
            }
            targets
        }
    };

    for integration_id in &integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "pull_request_review: failed to load project list");
                continue;
            }
        };
        let Some(probe_project) = first_project_with_reporter(state, &project_ids).await else {
            continue;
        };
        if !sender_is_trusted(state, probe_project, owner, repo, &reviewer).await {
            warn!(
                %integration_id,
                pr_number,
                %reviewer,
                "Ignoring PR review approval - reviewer is not a repo writer"
            );
            continue;
        }
        for project_id in &project_ids {
            if let Some(unparked) = unpark_pr_approval_eval(state, *project_id, pr_number).await {
                info!(
                    evaluation_id = %unparked.id,
                    pr_number,
                    %reviewer,
                    "PR approval gate cleared via native forge review"
                );
            }
        }
    }
}
