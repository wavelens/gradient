/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::commands::{
    active_task_ids_for_integration, first_task_with_reporter,
    github_installation_id_from_comment_body,
};
use super::installation::resolve_github_app_targets;
use super::payloads::GithubCheckRunRequestedAction;
use gradient_ci::{APPROVAL_ACTION_ID, find_approval_gated_eval, unpark_approval};
use gradient_core::ServerState;
use gradient_git_host::ParsedPullRequestReviewEvent;
use gradient_scheduler::Scheduler;
use gradient_types::events::evaluation;
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub(super) struct PullRequestApprovalContext {
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub is_fork: Option<bool>,
    pub sender: Option<String>,
}

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
    let Some(task_id) = eval.task else {
        return;
    };

    if !sender_is_trusted(state, task_id, owner, repo, sender.login).await {
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
            on_approval_granted(state, &unparked).await;
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

pub(super) async fn on_approval_granted(state: &Arc<ServerState>, eval: &MEvaluation) {
    let Some(task_id) = eval.task else {
        return;
    };
    state
        .record(evaluation::Reported {
            evaluation_id: eval.id,
            phase: evaluation::Phase::ApprovalGranted,
            status: i32::from(eval.status) as i16,
            task: Some(task_id),
            ..Default::default()
        })
        .await;
    state.record_evaluation_created(eval).await;
}

gradient_db::sql! {
    EVAL_BY_CHECK_RUN_ID = r#"SELECT id FROM evaluation
           WHERE check_run_ids IS NOT NULL
             AND EXISTS (
                 SELECT 1
                 FROM jsonb_each(check_run_ids) AS kv
                 WHERE (kv.value)::text::bigint = $1
             )
           LIMIT 1"#,
        params = [Int(123456)];
}

async fn find_eval_by_check_id(state: &Arc<ServerState>, check_id: i64) -> Option<MEvaluation> {
    // `evaluation.check_run_ids` is a JSON map per check-context name. Any stored id can match the
    // clicked check.
    use sea_orm::FromQueryResult;

    #[derive(FromQueryResult)]
    struct Row {
        id: uuid::Uuid,
    }

    let row =
        Row::find_by_statement(EVAL_BY_CHECK_RUN_ID.bind([sea_orm::Value::BigInt(Some(check_id))]))
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

/// The trust probe is failing closed on any error.
pub(super) async fn sender_is_trusted(
    state: &Arc<ServerState>,
    task_id: TaskId,
    owner: &str,
    repo: &str,
    sender: &str,
) -> bool {
    let reporter = match gradient_ci::actions::reporter_for_task(&state.ci(), task_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return false,
        Err(e) => {
            warn!(error = %e, %task_id, "resolving GitHostStatusReport action for trust probe");
            return false;
        }
    };
    match reporter.is_repo_writer(owner, repo, sender).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, %task_id, "is_repo_writer probe failed");
            false
        }
    }
}

async fn unpark_pr_approval_eval(
    state: &Arc<ServerState>,
    task_id: TaskId,
    pr_number: u64,
) -> Option<MEvaluation> {
    let eval = find_approval_gated_eval(&state.web_db, task_id, pr_number)
        .await
        .ok()
        .flatten()?;
    match unpark_approval(&state.web_db, eval.id).await {
        Ok(Some(unparked)) => {
            on_approval_granted(state, &unparked).await;
            Some(unparked)
        }
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate via review");
            None
        }
    }
}

pub(super) async fn handle_pull_request_review(
    state: &Arc<ServerState>,
    git_host: GitHostType,
    integration_id: Option<IntegrationId>,
    body: &[u8],
    client_ip: std::net::IpAddr,
) {
    let parsed = match git_host {
        GitHostType::GitHub => ParsedPullRequestReviewEvent::from_github(body),
        GitHostType::Gitea | GitHostType::Forgejo => ParsedPullRequestReviewEvent::from_gitea(body),
        GitHostType::GitLab => return,
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
        let task_ids = match active_task_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "pull_request_review: failed to load task list");
                continue;
            }
        };
        let Some(probe_task) = first_task_with_reporter(state, &task_ids).await else {
            continue;
        };
        if !sender_is_trusted(state, probe_task, owner, repo, &reviewer).await {
            warn!(
                %integration_id,
                pr_number,
                %reviewer,
                "Ignoring PR review approval - reviewer is not a repo writer"
            );
            continue;
        }
        for task_id in &task_ids {
            if let Some(unparked) = unpark_pr_approval_eval(state, *task_id, pr_number).await {
                info!(
                    evaluation_id = %unparked.id,
                    pr_number,
                    %reviewer,
                    "PR approval gate cleared via native Git host review"
                );
            }
        }
    }
}
