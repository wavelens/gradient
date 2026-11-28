/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::approval::{
    PullRequestApprovalContext, approve_approval_and_start_approved_commit_evaluation,
    login_can_push_to_repository,
};
use super::fanout::trigger_pr_for_connection;
use super::installation::{resolve_github_app_targets, task_serves_repository};
use super::payloads::{CommentPayload, GitlabNoteAttrs};
use super::response::TaskEvaluationNotStartedReason;
use gradient_ci::CiContext;
use gradient_ci::pull_request_summary::enable_pull_request_summary;
use gradient_db::WebDb;
use gradient_db::evaluations::branch::active_on_branch;
use gradient_db::fork_pull_request_approvals as approvals;
use gradient_scheduler::Scheduler;
use gradient_types::input::InputError;
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

struct CommentEvent {
    comment_body: String,
    pr_number: Option<u64>,
    sender_login: Option<String>,
    owner_repo: Option<String>,
    repository_urls: Vec<String>,
    comment_id: Option<i64>,
    comment_created_at: Option<chrono::NaiveDateTime>,
}

fn parse_comment_event(git_host: GitHostType, body: &[u8]) -> Option<CommentEvent> {
    let payload: CommentPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "comment webhook: failed to parse payload");
            return None;
        }
    };

    match git_host {
        GitHostType::GitLab => {
            let attrs = payload.object_attributes?;
            if attrs.noteable_type.as_deref() != Some("MergeRequest")
                || gitlab_note_webhook_reports_existing_comment_edit(&attrs)
            {
                return None;
            }
            let project = payload.project.unwrap_or_default();
            Some(CommentEvent {
                comment_created_at: attrs
                    .created_at
                    .as_deref()
                    .and_then(parse_webhook_comment_creation_time_to_utc_naive_datetime),
                comment_id: attrs.id,
                comment_body: attrs.note.unwrap_or_default(),
                pr_number: payload.merge_request.and_then(|m| m.iid),
                sender_login: payload.user.and_then(|u| u.username.or(u.login)),
                owner_repo: project.path_with_namespace,
                repository_urls: [project.web_url, project.git_http_url]
                    .into_iter()
                    .flatten()
                    .collect(),
            })
        }
        _ => {
            if payload.action.as_deref() != Some("created") {
                return None;
            }
            let comment = payload.comment.unwrap_or_default();
            let repository = payload.repository.unwrap_or_default();
            Some(CommentEvent {
                comment_created_at: comment
                    .created_at
                    .as_deref()
                    .and_then(parse_webhook_comment_creation_time_to_utc_naive_datetime),
                comment_id: comment.id,
                comment_body: comment.body.unwrap_or_default(),
                pr_number: payload
                    .pull_request
                    .or(payload.issue)
                    .and_then(|i| i.number),
                sender_login: payload.sender.and_then(|s| s.login.or(s.username)),
                owner_repo: repository.full_name,
                repository_urls: [repository.html_url, repository.clone_url]
                    .into_iter()
                    .flatten()
                    .collect(),
            })
        }
    }
}

fn gitlab_note_webhook_reports_existing_comment_edit(attrs: &GitlabNoteAttrs) -> bool {
    match (attrs.action.as_deref(), attrs.updated_at.as_deref()) {
        (Some(action), _) => action != "create",
        (None, None) => false,
        (None, Some(updated_at)) => {
            let created = attrs
                .created_at
                .as_deref()
                .and_then(parse_webhook_comment_creation_time_to_utc_naive_datetime);
            let updated = parse_webhook_comment_creation_time_to_utc_naive_datetime(updated_at);
            created.is_none() || created != updated
        }
    }
}

fn parse_webhook_comment_creation_time_to_utc_naive_datetime(
    created_at: &str,
) -> Option<chrono::NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|time| time.naive_utc())
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S UTC"))
        .ok()
}

pub(super) async fn handle_issue_comment(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    git_host: GitHostType,
    connection_id: Option<GitHostConnectionId>,
    body: &[u8],
    client_ip: std::net::IpAddr,
) {
    let comment_webhook_received_at = gradient_types::now();
    let Some(event) = parse_comment_event(git_host, body) else {
        return;
    };
    let approval_comment_time = comment_time_used_for_approval_decision(
        event.comment_created_at,
        comment_webhook_received_at,
    );
    let cmd = match parse_gradient_command(&event.comment_body) {
        Some(cmd) => cmd,
        None => return,
    };
    let Some(pr_number) = event.pr_number else {
        warn!("comment webhook: /gradient command but no PR number");
        return;
    };
    let Some(sender) = event.sender_login else {
        warn!("comment webhook: /gradient command but no sender");
        return;
    };
    let Some(owner_repo) = event.owner_repo else {
        warn!("comment webhook: /gradient command but no repo path");
        return;
    };
    let Some((owner, repo)) = owner_repo.rsplit_once('/') else {
        warn!(owner_repo, "comment webhook: malformed repo path");
        return;
    };

    let Some(connection_ids) =
        resolve_comment_connections(&ci.db.web_db, connection_id, body, &owner_repo, client_ip)
            .await
    else {
        return;
    };

    let (wildcard_override, wildcard_parse_error) = match resolve_wildcard_override(&cmd) {
        Ok(wildcard) => (wildcard, None),
        Err(e) => (None, Some(e)),
    };

    let reaction_target = event.comment_id.map(|id| gradient_ci::ReactionTarget {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr_number,
        comment_id: id,
    });

    let mut action_taken = false;
    for connection_id in &connection_ids {
        let route = CommentRoute {
            ci,
            scheduler,
            connection_id: *connection_id,
            cmd: &cmd,
            owner,
            repo,
            repository_urls: &event.repository_urls,
            sender: &sender,
            pr_number,
            wildcard_override: &wildcard_override,
            wildcard_parse_error: wildcard_parse_error.as_ref(),
            reaction_target: &reaction_target,
            approval_comment_time,
        };
        if handle_comment_for_connection(&route).await {
            action_taken = true;
        }
    }
    if !action_taken {
        debug!(pr_number, "The /gradient comment ran no operation.");
    }
}

async fn resolve_comment_connections(
    db: &WebDb,
    connection_id: Option<GitHostConnectionId>,
    body: &[u8],
    owner_repo: &str,
    client_ip: std::net::IpAddr,
) -> Option<Vec<GitHostConnectionId>> {
    if let Some(id) = connection_id {
        return Some(vec![id]);
    }
    let Some(installation_id) = github_installation_id_from_comment_body(body) else {
        warn!("comment webhook (github): no installation_id");
        return None;
    };
    let targets = resolve_github_app_targets(db, installation_id, client_ip).await;
    if targets.is_empty() {
        warn!(installation_id, %owner_repo, "comment webhook (github): the installation has no connection to a Git host");
        return None;
    }
    Some(targets)
}

fn resolve_wildcard_override(cmd: &GradientCommand) -> Result<Option<String>, InputError> {
    let GradientCommand::Run {
        wildcard: Some(raw),
        ..
    } = cmd
    else {
        return Ok(None);
    };
    raw.parse::<Wildcard>().map(|_| Some(raw.clone()))
}

struct CommentRoute<'a> {
    ci: &'a CiContext,
    scheduler: &'a Arc<Scheduler>,
    connection_id: GitHostConnectionId,
    cmd: &'a GradientCommand,
    owner: &'a str,
    repo: &'a str,
    repository_urls: &'a [String],
    sender: &'a str,
    pr_number: u64,
    wildcard_override: &'a Option<String>,
    wildcard_parse_error: Option<&'a InputError>,
    reaction_target: &'a Option<gradient_ci::ReactionTarget>,
    approval_comment_time: Option<chrono::NaiveDateTime>,
}

async fn handle_comment_for_connection(ctx: &CommentRoute<'_>) -> bool {
    let task_ids = match load_active_pull_request_tasks_for_event_repository(
        &ctx.ci.db.web_db,
        ctx.connection_id,
        ctx.owner,
        ctx.repo,
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "comment webhook: failed to load task list");
            return false;
        }
    };
    let Some(probe_task) = first_task_with_reporter(ctx.ci, &task_ids).await else {
        warn!(
            connection_id = %ctx.connection_id,
            pr_number = ctx.pr_number,
            tasks = task_ids.len(),
            "/gradient comment ignored: no task of this connection reports commit status"
        );
        return false;
    };
    let is_maintainer =
        login_can_push_to_repository(ctx.ci, probe_task, ctx.owner, ctx.repo, ctx.sender).await;
    if let Some(target) = ctx.reaction_target {
        let kind = if is_maintainer {
            gradient_ci::ReactionKind::Eyes
        } else {
            gradient_ci::ReactionKind::Confused
        };
        fire_reaction_via_task(ctx.ci, probe_task, target, kind).await;
    }
    match comment_command_action_from_push_permission_and_parse_error(
        is_maintainer,
        ctx.wildcard_parse_error,
    ) {
        PullRequestCommentCommandOutcome::CommenterCannotPushIgnoreCommand => {
            warn!(
                connection_id = %ctx.connection_id,
                pr_number = ctx.pr_number,
                sender = %ctx.sender,
                "Rejecting /gradient command - sender is not a repo writer"
            );
            return false;
        }
        PullRequestCommentCommandOutcome::MaintainerWildcardParseError(parse_error) => {
            warn!(error = %parse_error, "/gradient run wildcard rejected");
            post_wildcard_error_comment(
                ctx.ci,
                probe_task,
                ctx.owner,
                ctx.repo,
                ctx.pr_number,
                parse_error,
            )
            .await;
            return false;
        }
        PullRequestCommentCommandOutcome::ValidMaintainerCommand => {}
    }

    match ctx.cmd {
        GradientCommand::Summary => return enable_summaries(ctx, &task_ids).await,
        GradientCommand::Abort => {
            return abort_running_evaluations_for_pull_request(ctx, &task_ids).await;
        }
        GradientCommand::Run { .. } | GradientCommand::Approve { .. } => {}
    }
    let Some(snapshot) =
        fetch_pr_snapshot(ctx.ci, &task_ids, ctx.owner, ctx.repo, ctx.pr_number).await
    else {
        warn!(
            connection_id = %ctx.connection_id,
            pr_number = ctx.pr_number,
            "/gradient command: failed to fetch pull request head, so nothing released or started."
        );
        return false;
    };
    let head_sha = match hex::decode(&snapshot.head_sha) {
        Ok(sha) => sha,
        Err(e) => {
            warn!(error = %e, sha = %snapshot.head_sha, "/gradient command: pull request head SHA is not valid hex");
            return false;
        }
    };
    let Some(pending_commits) =
        load_pending_maintainer_approval_commit_hashes_for_pull_request_tasks(ctx, &task_ids).await
    else {
        return false;
    };
    match decide_allowed_comment_action(
        ctx.cmd,
        &head_sha,
        &pending_commits,
        ctx.approval_comment_time,
    ) {
        MaintainerCommentCommandDecision::ReleaseEvaluationsWaitingForMaintainerApproval(
            commit,
        ) => approve_fork_pull_request_commit_and_start_evaluations(ctx, &task_ids, &commit).await,
        MaintainerCommentCommandDecision::StartPullRequestHeadCommitEvaluation => {
            run_fresh_evaluation(ctx, probe_task, snapshot, head_sha).await
        }
        MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit => {
            post_refusal_reply_comment_with_command_naming_head_commit(
                ctx,
                probe_task,
                &snapshot.head_sha,
            )
            .await;
            false
        }
        MaintainerCommentCommandDecision::RefuseBareApproveBecauseHeadCommitWasRecordedLessThanSixtySecondsBeforeComment {
            pull_request_head_commit_hash,
            head_commit_recorded_at,
        } => {
            warn!(
                connection_id = %ctx.connection_id,
                pr_number = ctx.pr_number,
                head = %snapshot.head_sha,
                %head_commit_recorded_at,
                "Refused `/gradient approve` without a commit hash: Gradient recorded the pull request head commit less than 60 seconds before the comment."
            );
            post_refusal_reply_comment_on_pull_request(
                ctx,
                probe_task,
                &build_bare_approve_reply_comment_when_head_commit_recorded_too_recently(
                    &pull_request_head_commit_hash,
                    head_commit_recorded_at,
                ),
            )
            .await;
            false
        }
        MaintainerCommentCommandDecision::NoEvaluationWaitsForApproval => false,
    }
}

async fn post_refusal_reply_comment_with_command_naming_head_commit(
    ctx: &CommentRoute<'_>,
    probe_task: TaskId,
    head_sha: &str,
) {
    warn!(
        connection_id = %ctx.connection_id,
        pr_number = ctx.pr_number,
        head = %head_sha,
        "Refused `/gradient` command. The comment can lack a commit, use an abbreviated commit hash, or name a commit other than the pull request head."
    );
    post_refusal_reply_comment_on_pull_request(
        ctx,
        probe_task,
        &build_reply_comment_for_commit_not_matching_head(ctx.cmd, head_sha),
    )
    .await;
}

#[derive(Debug, PartialEq)]
enum PullRequestCommentCommandOutcome<'a> {
    CommenterCannotPushIgnoreCommand,
    MaintainerWildcardParseError(&'a InputError),
    ValidMaintainerCommand,
}

fn comment_command_action_from_push_permission_and_parse_error(
    commenter_can_push: bool,
    wildcard_parse_error: Option<&InputError>,
) -> PullRequestCommentCommandOutcome<'_> {
    match (commenter_can_push, wildcard_parse_error) {
        (false, _) => PullRequestCommentCommandOutcome::CommenterCannotPushIgnoreCommand,
        (true, Some(parse_error)) => {
            PullRequestCommentCommandOutcome::MaintainerWildcardParseError(parse_error)
        }
        (true, None) => PullRequestCommentCommandOutcome::ValidMaintainerCommand,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MaintainerCommentCommandDecision {
    ReleaseEvaluationsWaitingForMaintainerApproval(Vec<u8>),
    StartPullRequestHeadCommitEvaluation,
    RefuseAndReplyWithCommandNamingHeadCommit,
    RefuseBareApproveBecauseHeadCommitWasRecordedLessThanSixtySecondsBeforeComment {
        pull_request_head_commit_hash: Vec<u8>,
        head_commit_recorded_at: chrono::NaiveDateTime,
    },
    NoEvaluationWaitsForApproval,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingForkPullRequestApprovalCommitAndGradientRecordedAt {
    commit_hash: Vec<u8>,
    waiting_commit_recorded_at: chrono::NaiveDateTime,
}

const MINIMUM_SECONDS_BETWEEN_PULL_REQUEST_HEAD_COMMIT_AND_BARE_APPROVE_COMMENT: i64 = 60;

fn comment_time_used_for_approval_decision(
    comment_created_at: Option<chrono::NaiveDateTime>,
    comment_webhook_received_at: chrono::NaiveDateTime,
) -> Option<chrono::NaiveDateTime> {
    comment_created_at.map(|created_at| created_at.min(comment_webhook_received_at))
}

fn decide_allowed_comment_action(
    cmd: &GradientCommand,
    head_sha: &[u8],
    pending_approvals: &[PendingForkPullRequestApprovalCommitAndGradientRecordedAt],
    approval_comment_time: Option<chrono::NaiveDateTime>,
) -> MaintainerCommentCommandDecision {
    let named_commit = match cmd.commit_named_in_comment() {
        CommitSpecifiedInCommentCommand::AbbreviatedCommitHash => {
            return MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit;
        }
        CommitSpecifiedInCommentCommand::FullCommitHash(hash) => Some(hash),
        CommitSpecifiedInCommentCommand::NoCommitHashSpecified => None,
    };
    if let Some(commit) = named_commit
        && pending_approvals
            .iter()
            .any(|pending| &pending.commit_hash == commit)
    {
        return MaintainerCommentCommandDecision::ReleaseEvaluationsWaitingForMaintainerApproval(
            commit.clone(),
        );
    }
    match (cmd, named_commit) {
        (GradientCommand::Run { .. }, None) => {
            MaintainerCommentCommandDecision::StartPullRequestHeadCommitEvaluation
        }
        (GradientCommand::Run { .. }, Some(commit)) if commit.as_slice() == head_sha => {
            MaintainerCommentCommandDecision::StartPullRequestHeadCommitEvaluation
        }
        (GradientCommand::Approve { .. }, _) if pending_approvals.is_empty() => {
            MaintainerCommentCommandDecision::NoEvaluationWaitsForApproval
        }
        (GradientCommand::Approve { .. }, None) => {
            decide_approve_comment_without_commit_if_pending_approval_is_at_least_60_seconds_old(
                head_sha,
                pending_approvals,
                approval_comment_time,
            )
        }
        _ => MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit,
    }
}

fn decide_approve_comment_without_commit_if_pending_approval_is_at_least_60_seconds_old(
    head_sha: &[u8],
    pending_approvals: &[PendingForkPullRequestApprovalCommitAndGradientRecordedAt],
    approval_comment_time: Option<chrono::NaiveDateTime>,
) -> MaintainerCommentCommandDecision {
    let head_commit_recorded_at = pending_approvals
        .iter()
        .filter(|pending| pending.commit_hash.as_slice() == head_sha)
        .map(|pending| pending.waiting_commit_recorded_at)
        .max();
    let (Some(head_commit_recorded_at), Some(approval_comment_time)) =
        (head_commit_recorded_at, approval_comment_time)
    else {
        return MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit;
    };
    let minimum_wait_time = chrono::TimeDelta::seconds(
        MINIMUM_SECONDS_BETWEEN_PULL_REQUEST_HEAD_COMMIT_AND_BARE_APPROVE_COMMENT,
    );
    if approval_comment_time - head_commit_recorded_at >= minimum_wait_time {
        MaintainerCommentCommandDecision::ReleaseEvaluationsWaitingForMaintainerApproval(
            head_sha.to_vec(),
        )
    } else {
        MaintainerCommentCommandDecision::RefuseBareApproveBecauseHeadCommitWasRecordedLessThanSixtySecondsBeforeComment {
            pull_request_head_commit_hash: head_sha.to_vec(),
            head_commit_recorded_at,
        }
    }
}

async fn load_pending_maintainer_approval_commit_hashes_for_pull_request_tasks(
    ctx: &CommentRoute<'_>,
    task_ids: &[TaskId],
) -> Option<Vec<PendingForkPullRequestApprovalCommitAndGradientRecordedAt>> {
    let mut pending_commits = Vec::new();
    for task_id in task_ids {
        match approvals::get_newest_pending_pull_request_approval_for_task(
            &ctx.ci.db.web_db,
            *task_id,
            ctx.pr_number,
        )
        .await
        {
            Ok(pending) => pending_commits.extend(pending.map(|pending| {
                PendingForkPullRequestApprovalCommitAndGradientRecordedAt {
                    commit_hash: pending.commit_hash,
                    waiting_commit_recorded_at: pending.created_at,
                }
            })),
            Err(e) => {
                warn!(error = %e, %task_id, "/gradient command: failed to load pending maintainer approvals for the pull request");
                return None;
            }
        }
    }
    Some(pending_commits)
}

fn build_reply_comment_for_commit_not_matching_head(
    cmd: &GradientCommand,
    head_sha: &str,
) -> String {
    let head_sha = head_sha.to_ascii_lowercase();
    let command = match cmd {
        GradientCommand::Run {
            wildcard: Some(wildcard),
            ..
        } => format!("/gradient run {head_sha} {wildcard}"),
        GradientCommand::Run { .. } => format!("/gradient run {head_sha}"),
        _ => format!("/gradient approve {head_sha}"),
    };
    format!(
        "Gradient did not run anything. The comment named no commit, a short hash, or a commit other than the pull request HEAD. Use a full 40-character commit hash in the command. Use the comment below to target the current HEAD commit.\n\n```\n{command}\n```"
    )
}

fn build_bare_approve_reply_comment_when_head_commit_recorded_too_recently(
    head_commit_hash: &[u8],
    head_commit_recorded_at: chrono::NaiveDateTime,
) -> String {
    let head = hex::encode(head_commit_hash);
    let recorded_at = head_commit_recorded_at.format("%Y-%m-%d %H:%M:%S");
    let margin = MINIMUM_SECONDS_BETWEEN_PULL_REQUEST_HEAD_COMMIT_AND_BARE_APPROVE_COMMENT;
    format!(
        "Gradient approved nothing. Gradient recorded {head}, the HEAD commit of the pull request, at {recorded_at} UTC. The gap to this comment is less than {margin} seconds. Commands to approve without a commit hash need a HEAD commit that Gradient recorded at least {margin} seconds before this comment. Use the comment below to approve this commit.\n\n```\n/gradient approve {head}\n```"
    )
}

async fn post_refusal_reply_comment_on_pull_request(
    ctx: &CommentRoute<'_>,
    task_id: TaskId,
    body: &str,
) {
    let reporter = match gradient_ci::task_commit_status_posting::reporter_for_task(ctx.ci, task_id)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %task_id, "/gradient command: resolving reporter for reply comment");
            return;
        }
    };
    if let Err(e) = reporter
        .post_pr_comment(ctx.owner, ctx.repo, ctx.pr_number, body)
        .await
    {
        warn!(error = %e, %task_id, "/gradient command: failed to post the reply comment");
    }
}

fn build_source_comment_for_pull_request_command(
    target: Option<&gradient_ci::ReactionTarget>,
) -> Option<serde_json::Value> {
    target.map(|t| {
        serde_json::json!({
            "owner": t.owner,
            "repo": t.repo,
            "pr_number": t.pr_number,
            "comment_id": t.comment_id,
        })
    })
}

async fn enable_summaries(ctx: &CommentRoute<'_>, task_ids: &[TaskId]) -> bool {
    let mut enabled_any = false;
    for task_id in task_ids {
        let enabled =
            enable_pull_request_summary(ctx.ci, *task_id, ctx.owner, ctx.repo, ctx.pr_number).await;
        match enabled {
            Ok(enabled) => enabled_any |= enabled,
            Err(e) => {
                warn!(error = %e, %task_id, pr_number = ctx.pr_number, "/gradient summary failed")
            }
        }
    }

    enabled_any
}

async fn abort_running_evaluations_for_pull_request(
    ctx: &CommentRoute<'_>,
    task_ids: &[TaskId],
) -> bool {
    let pull_request_branch = branch_key::pull_request(ctx.pr_number);
    let mut aborted_any = false;
    for task_id in task_ids {
        let running = match active_on_branch(&ctx.ci.db.web_db, *task_id, &pull_request_branch)
            .await
        {
            Ok(running) => running,
            Err(e) => {
                warn!(error = %e, %task_id, pr_number = ctx.pr_number, "/gradient abort: failed to load the running evaluations of the pull request");
                continue;
            }
        };
        for evaluation in running {
            info!(evaluation_id = %evaluation.id, %task_id, pr_number = ctx.pr_number, "/gradient abort: Aborting the evaluation still running for the pull request");
            ctx.scheduler.abort_evaluation(evaluation, None).await;
            aborted_any = true;
        }
    }
    aborted_any
}

async fn approve_fork_pull_request_commit_and_start_evaluations(
    ctx: &CommentRoute<'_>,
    task_ids: &[TaskId],
    head_sha: &[u8],
) -> bool {
    let mut started_any = false;
    for task_id in task_ids {
        let pending = match approvals::get_pending_pull_request_approval_for_task_commit(
            &ctx.ci.db.web_db,
            *task_id,
            ctx.pr_number,
            head_sha,
        )
        .await
        {
            Ok(Some(pending)) => pending,
            Ok(_) => continue,
            Err(e) => {
                warn!(error = %e, %task_id, "Failed to start the evaluation pending maintainer approval.");
                continue;
            }
        };
        started_any |= approve_approval_and_start_approved_commit_evaluation(
            ctx.ci,
            ctx.scheduler,
            pending.id,
            ctx.sender,
            ctx.wildcard_override.clone(),
            build_source_comment_for_pull_request_command(ctx.reaction_target.as_ref()),
        )
        .await;
    }
    started_any
}

/// Only a comment with the full hash of the head commit can approve that commit on a fork pull request. Commands without the hash leave the decision to the fork gate. Fork commits that need approval wait for approval.
async fn run_fresh_evaluation(
    ctx: &CommentRoute<'_>,
    probe_task: TaskId,
    snapshot: gradient_ci::PullRequestSnapshot,
    commit_hash: Vec<u8>,
) -> bool {
    let approval_ctx = build_approval_context_for_gradient_run_comment(
        ctx.pr_number,
        &snapshot,
        ctx.cmd.commit_named_in_comment(),
        &commit_hash,
    );
    if ctx.repository_urls.is_empty() {
        warn!(
            connection_id = %ctx.connection_id,
            pr_number = ctx.pr_number,
            "/gradient run: no repository URL in the comment payload"
        );
        return false;
    }
    let outcome = trigger_pr_for_connection(
        ctx.ci,
        ctx.scheduler,
        ctx.connection_id,
        ctx.repository_urls,
        Some(snapshot.head_branch.as_str()),
        None,
        "synchronize",
        commit_hash,
        snapshot.title.clone(),
        Some(ctx.sender.to_string()),
        approval_ctx,
        snapshot.head_clone_url.clone(),
        true,
        ctx.wildcard_override.clone(),
        build_source_comment_for_pull_request_command(ctx.reaction_target.as_ref()),
    )
    .await;
    if outcome.skipped.iter().any(|skipped| {
        matches!(
            skipped.reason,
            TaskEvaluationNotStartedReason::ForkPullRequestCommitWaitsForMaintainerApproval
        )
    }) {
        post_refusal_reply_comment_with_command_naming_head_commit(
            ctx,
            probe_task,
            &snapshot.head_sha,
        )
        .await;
    }
    if !outcome.queued.is_empty() {
        info!(
            connection_id = %ctx.connection_id,
            pr_number = ctx.pr_number,
            sender = %ctx.sender,
            wildcard_override = ctx.wildcard_override.as_deref(),
            queued = outcome.queued.len(),
            "/gradient run created fresh evaluation"
        );
        true
    } else {
        debug!(
            connection_id = %ctx.connection_id,
            pr_number = ctx.pr_number,
            "/gradient run: trigger fan-out queued nothing"
        );
        false
    }
}

pub(super) fn build_approval_context_for_gradient_run_comment(
    pr_number: u64,
    snapshot: &gradient_ci::PullRequestSnapshot,
    commit_named_in_comment: &CommitSpecifiedInCommentCommand,
    commit_hash: &[u8],
) -> PullRequestApprovalContext {
    let approved_head_commit_sha = match commit_named_in_comment {
        CommitSpecifiedInCommentCommand::FullCommitHash(named)
            if named.as_slice() == commit_hash =>
        {
            Some(commit_hash.to_vec())
        }
        _ => None,
    };
    PullRequestApprovalContext {
        pr_number: Some(pr_number),
        pr_author: None,
        is_fork: snapshot.is_fork,
        approved_head_commit_sha,
        ..PullRequestApprovalContext::default()
    }
}

async fn fire_reaction_via_task(
    ci: &CiContext,
    task_id: TaskId,
    target: &gradient_ci::ReactionTarget,
    kind: gradient_ci::ReactionKind,
) {
    let reporter =
        match gradient_ci::task_commit_status_posting::reporter_for_task(ci, task_id).await {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                warn!(error = %e, %task_id, "/gradient reaction: resolving reporter");
                return;
            }
        };
    if let Err(e) = reporter.add_reaction(target, kind).await {
        warn!(error = %e, %task_id, ?kind, "/gradient reaction: post failed");
    }
}

pub(super) async fn first_task_with_reporter(
    ci: &CiContext,
    task_ids: &[TaskId],
) -> Option<TaskId> {
    for task_id in task_ids {
        if let Ok(Some(_)) =
            gradient_ci::task_commit_status_posting::reporter_for_task(ci, *task_id).await
        {
            return Some(*task_id);
        }
    }
    None
}

pub(super) async fn load_active_pull_request_tasks_for_event_repository(
    db: &WebDb,
    connection_id: GitHostConnectionId,
    owner: &str,
    repo: &str,
) -> Result<Vec<TaskId>, sea_orm::DbErr> {
    let task_ids = gradient_db::triggers::active_pull_request_task_ids_using_git_host_connection(
        db,
        connection_id,
    )
    .await?;
    let tasks = gradient_db::lookup::by_ids::<ETask, _>(db, &task_ids).await?;
    Ok(filter_task_ids_by_repository_url_owner_and_name(
        task_ids, &tasks, owner, repo,
    ))
}

fn filter_task_ids_by_repository_url_owner_and_name(
    task_ids: Vec<TaskId>,
    tasks: &HashMap<TaskId, MTask>,
    owner: &str,
    repo: &str,
) -> Vec<TaskId> {
    task_ids
        .into_iter()
        .filter(|id| {
            tasks
                .get(id)
                .is_some_and(|task| task_serves_repository(&task.repository, owner, repo))
        })
        .collect()
}

async fn fetch_pr_snapshot(
    ci: &CiContext,
    task_ids: &[TaskId],
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Option<gradient_ci::PullRequestSnapshot> {
    for task_id in task_ids {
        let reporter =
            match gradient_ci::task_commit_status_posting::reporter_for_task(ci, *task_id).await {
                Ok(Some(r)) => r,
                _ => continue,
            };
        match reporter.get_pull_request(owner, repo, pr_number).await {
            Ok(Some(snap)) => return Some(snap),
            Ok(None) => return None,
            Err(e) => {
                warn!(error = %e, %task_id, "/gradient run: PR fetch failed, trying next task");
            }
        }
    }
    None
}

fn build_invalid_gradient_run_wildcard_reply_comment(parse_error: &InputError) -> String {
    format!(
        "Failed to parse the wildcard of `/gradient run`: {}",
        parse_error
    )
}

async fn post_wildcard_error_comment(
    ci: &CiContext,
    task_id: TaskId,
    owner: &str,
    repo: &str,
    pr_number: u64,
    parse_error: &InputError,
) {
    let reporter = match gradient_ci::task_commit_status_posting::reporter_for_task(ci, task_id)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %task_id, "/gradient run wildcard: resolving reporter for reply comment");
            return;
        }
    };
    let body = build_invalid_gradient_run_wildcard_reply_comment(parse_error);
    if let Err(e) = reporter
        .post_pr_comment(owner, repo, pr_number, &body)
        .await
    {
        warn!(error = %e, %task_id, "/gradient run wildcard: failed to post the reply comment");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GradientCommand {
    Run {
        commit_named_in_comment: CommitSpecifiedInCommentCommand,
        wildcard: Option<String>,
    },
    Approve {
        commit_named_in_comment: CommitSpecifiedInCommentCommand,
    },
    Summary,
    Abort,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CommitSpecifiedInCommentCommand {
    NoCommitHashSpecified,
    FullCommitHash(Vec<u8>),
    AbbreviatedCommitHash,
}

impl GradientCommand {
    fn commit_named_in_comment(&self) -> &CommitSpecifiedInCommentCommand {
        match self {
            GradientCommand::Run {
                commit_named_in_comment,
                ..
            }
            | GradientCommand::Approve {
                commit_named_in_comment,
            } => commit_named_in_comment,
            GradientCommand::Summary | GradientCommand::Abort => {
                &CommitSpecifiedInCommentCommand::NoCommitHashSpecified
            }
        }
    }
}

fn parse_commit_from_comment_word(word: &str) -> Option<CommitSpecifiedInCommentCommand> {
    if !word.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match word.len() {
        40 => hex::decode(word)
            .ok()
            .map(CommitSpecifiedInCommentCommand::FullCommitHash),
        7..40 => Some(CommitSpecifiedInCommentCommand::AbbreviatedCommitHash),
        _ => None,
    }
}

/// The command must be on its own line. Blank lines and `> ...` quote-reply lines are skipped. Any
/// other prose is disqualifying the comment.
pub(super) fn parse_gradient_command(body: &str) -> Option<GradientCommand> {
    const PREFIX: &str = "/gradient";

    let mut found: Option<GradientCommand> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('>') {
            continue;
        }
        if found.is_some() {
            return None;
        }
        if trimmed.len() < PREFIX.len() {
            return None;
        }
        let (prefix, rest) = trimmed.split_at(PREFIX.len());
        if !prefix.eq_ignore_ascii_case(PREFIX) {
            return None;
        }
        if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
            return None;
        }
        let rest = rest.trim();
        let (verb, arg) = match rest.split_once(|c: char| c.is_ascii_whitespace()) {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };
        let (first_arg, after_first_arg) = match arg.split_once(|c: char| c.is_ascii_whitespace()) {
            Some((first, after)) => (first, after.trim()),
            None => (arg, ""),
        };
        let commit = parse_commit_from_comment_word(first_arg);
        if verb.eq_ignore_ascii_case("run") {
            let wildcard = if commit.is_some() {
                after_first_arg
            } else {
                arg
            };
            found = Some(GradientCommand::Run {
                commit_named_in_comment: commit
                    .unwrap_or(CommitSpecifiedInCommentCommand::NoCommitHashSpecified),
                wildcard: (!wildcard.is_empty()).then(|| wildcard.to_string()),
            });
            continue;
        }
        if verb.eq_ignore_ascii_case("approve") {
            if !after_first_arg.is_empty() || (commit.is_none() && !arg.is_empty()) {
                return None;
            }
            found = Some(GradientCommand::Approve {
                commit_named_in_comment: commit
                    .unwrap_or(CommitSpecifiedInCommentCommand::NoCommitHashSpecified),
            });
            continue;
        }
        let bare = if verb.eq_ignore_ascii_case("summary") {
            GradientCommand::Summary
        } else if verb.eq_ignore_ascii_case("abort") {
            GradientCommand::Abort
        } else {
            return None;
        };
        if !arg.is_empty() {
            return None;
        }
        found = Some(bare);
    }
    found
}

pub(super) fn github_installation_id_from_comment_body(body: &[u8]) -> Option<i64> {
    #[derive(Deserialize)]
    struct WithInstallation {
        installation: Option<InstallationId>,
    }
    #[derive(Deserialize)]
    struct InstallationId {
        id: i64,
    }
    serde_json::from_slice::<WithInstallation>(body)
        .ok()
        .and_then(|p| p.installation)
        .map(|i| i.id)
}

#[cfg(test)]
mod tests {
    use super::CommitSpecifiedInCommentCommand::{
        AbbreviatedCommitHash, FullCommitHash, NoCommitHashSpecified,
    };
    use super::{
        CommentEvent, CommitSpecifiedInCommentCommand, GradientCommand,
        MaintainerCommentCommandDecision,
        PendingForkPullRequestApprovalCommitAndGradientRecordedAt,
        PullRequestCommentCommandOutcome,
        build_bare_approve_reply_comment_when_head_commit_recorded_too_recently,
        build_invalid_gradient_run_wildcard_reply_comment,
        build_reply_comment_for_commit_not_matching_head,
        comment_command_action_from_push_permission_and_parse_error,
        comment_time_used_for_approval_decision, decide_allowed_comment_action,
        filter_task_ids_by_repository_url_owner_and_name, parse_comment_event,
        parse_commit_from_comment_word, parse_gradient_command,
    };
    use gradient_types::wildcard::Wildcard;
    use gradient_types::{GitHostType, MTask, TaskId};
    use std::collections::HashMap;

    #[test]
    fn approval_tasks_stay_in_the_event_repository() {
        let task = |n: u128, repository: &str| MTask {
            id: TaskId::new(uuid::Uuid::from_u128(n)),
            repository: repository.to_string(),
            ..Default::default()
        };
        let repo_a = task(1, "https://github.com/acme/A.git");
        let repo_b = task(2, "git@github.com:acme/b.git");
        let task_ids = vec![repo_a.id, repo_b.id];
        let tasks = HashMap::from([(repo_a.id, repo_a.clone()), (repo_b.id, repo_b.clone())]);

        assert_eq!(
            filter_task_ids_by_repository_url_owner_and_name(task_ids.clone(), &tasks, "acme", "a"),
            vec![repo_a.id]
        );
        assert_eq!(
            filter_task_ids_by_repository_url_owner_and_name(task_ids, &tasks, "acme", "b"),
            vec![repo_b.id]
        );
    }

    fn event_of(git_host: GitHostType, value: serde_json::Value) -> CommentEvent {
        parse_comment_event(git_host, &serde_json::to_vec(&value).unwrap())
            .expect("payload should parse into a CommentEvent")
    }

    #[test]
    fn parse_comment_event_github_issue_comment() {
        let e = event_of(
            GitHostType::GitHub,
            serde_json::json!({
                "action": "created",
                "comment": { "id": 555, "body": "/gradient run" },
                "issue": { "number": 42 },
                "sender": { "login": "octocat" },
                "repository": {
                    "full_name": "octo/repo",
                    "html_url": "https://github.example/octo/repo",
                    "clone_url": "https://github.example/octo/repo.git",
                },
            }),
        );
        assert_eq!(
            e.repository_urls,
            [
                "https://github.example/octo/repo",
                "https://github.example/octo/repo.git"
            ]
        );
        assert_eq!(e.comment_body, "/gradient run");
        assert_eq!(e.pr_number, Some(42));
        assert_eq!(e.sender_login.as_deref(), Some("octocat"));
        assert_eq!(e.owner_repo.as_deref(), Some("octo/repo"));
        assert_eq!(e.comment_id, Some(555));
    }

    #[test]
    fn parse_comment_event_gitea_prefers_pull_request_and_username() {
        let e = event_of(
            GitHostType::Gitea,
            serde_json::json!({
                "action": "created",
                "comment": { "id": 99, "body": "/gradient approve" },
                "pull_request": { "number": 7 },
                "sender": { "username": "gitea-user" },
                "repository": { "full_name": "acme/widgets" },
            }),
        );
        assert_eq!(e.comment_body, "/gradient approve");
        assert_eq!(e.pr_number, Some(7));
        assert_eq!(e.sender_login.as_deref(), Some("gitea-user"));
        assert_eq!(e.owner_repo.as_deref(), Some("acme/widgets"));
        assert_eq!(e.comment_id, Some(99));
    }

    #[test]
    fn parse_comment_event_gitlab_note() {
        let e = event_of(
            GitHostType::GitLab,
            serde_json::json!({
                "object_attributes": {
                    "id": 123,
                    "note": "/gradient run",
                    "noteable_type": "MergeRequest",
                },
                "merge_request": { "iid": 5 },
                "user": { "username": "gl-user" },
                "project": {
                    "path_with_namespace": "group/proj",
                    "web_url": "https://gitlab.example/group/proj",
                },
            }),
        );
        assert_eq!(e.repository_urls, ["https://gitlab.example/group/proj"]);
        assert_eq!(e.comment_body, "/gradient run");
        assert_eq!(e.pr_number, Some(5));
        assert_eq!(e.sender_login.as_deref(), Some("gl-user"));
        assert_eq!(e.owner_repo.as_deref(), Some("group/proj"));
        assert_eq!(e.comment_id, Some(123));
    }

    #[test]
    fn parse_comment_event_github_ignores_non_created_action() {
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "edited",
            "comment": { "id": 1, "body": "/gradient run" },
            "issue": { "number": 1 },
            "sender": { "login": "octocat" },
            "repository": { "full_name": "octo/repo" },
        }))
        .unwrap();
        assert!(parse_comment_event(GitHostType::GitHub, &body).is_none());
    }

    #[test]
    fn github_edited_comment_with_gradient_approve_produces_no_comment_event() {
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "edited",
            "comment": { "id": 1, "body": "/gradient approve" },
            "issue": { "number": 1 },
            "sender": { "login": "octocat" },
            "repository": { "full_name": "octo/repo" },
        }))
        .unwrap();
        assert!(parse_comment_event(GitHostType::GitHub, &body).is_none());
    }

    fn gitlab_merge_request_comment_event_from_object_attributes_json(
        attributes: serde_json::Value,
    ) -> Option<CommentEvent> {
        let mut object_attributes = serde_json::json!({
            "id": 1,
            "note": "/gradient approve",
            "noteable_type": "MergeRequest",
        });
        object_attributes
            .as_object_mut()
            .unwrap()
            .extend(attributes.as_object().unwrap().clone());
        let body = serde_json::to_vec(&serde_json::json!({
            "object_attributes": object_attributes,
            "merge_request": { "iid": 1 },
            "user": { "username": "maintainer" },
            "project": { "path_with_namespace": "group/proj" },
        }))
        .unwrap();
        parse_comment_event(GitHostType::GitLab, &body)
    }

    #[test]
    fn gitlab_comment_without_action_is_new_command_when_updated_at_equals_or_is_missing_created_at_and_is_edit_when_different()
     {
        let created_at = "2026-11-19T10:15:00Z";
        assert!(
            gitlab_merge_request_comment_event_from_object_attributes_json(serde_json::json!({
                "created_at": created_at,
                "updated_at": "2026-11-19T10:20:00Z",
            }))
            .is_none()
        );
        for attributes in [
            serde_json::json!({ "created_at": created_at, "updated_at": created_at }),
            serde_json::json!({ "created_at": created_at }),
        ] {
            let event =
                gitlab_merge_request_comment_event_from_object_attributes_json(attributes.clone())
                    .unwrap_or_else(|| panic!("{attributes}"));
            assert_eq!(event.comment_body, "/gradient approve");
            assert_eq!(
                event.comment_created_at,
                Some(utc_date_time_2026_11_19(10, 15, 0))
            );
        }
    }

    #[test]
    fn gitlab_bare_approve_without_action_or_created_at_is_refused_and_reply_names_head_commit() {
        let event =
            gitlab_merge_request_comment_event_from_object_attributes_json(serde_json::json!({}))
                .expect("a note without action and without times is read as a command");
        assert_eq!(event.comment_created_at, None);
        let approval_comment_time = comment_time_used_for_approval_decision(
            event.comment_created_at,
            fixed_test_comment_time(),
        );
        let long_ago = utc_date_time_2026_11_19(9, 0, 0);
        assert_eq!(
            decide_allowed_comment_action(
                &parse_gradient_command(&event.comment_body).unwrap(),
                &COMMIT_Y,
                &[pending_approval_for_commit_recorded_at(COMMIT_Y, long_ago)],
                approval_comment_time,
            ),
            MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit
        );
        let reply = build_reply_comment_for_commit_not_matching_head(
            &approve(None),
            &hex::encode(COMMIT_Y),
        );
        assert!(
            reply.contains(&format!("/gradient approve {}", "bb".repeat(20))),
            "{reply}"
        );
    }

    #[test]
    fn gitlab_note_action_decides_new_comment_or_edit_ignoring_timestamps() {
        let edited_times = serde_json::json!({
            "created_at": "2026-11-19T10:15:00Z",
            "updated_at": "2026-11-19T10:20:00Z",
        });
        for (action, read_as_command) in [("create", true), ("update", false)] {
            let mut attributes = edited_times.clone();
            attributes["action"] = serde_json::json!(action);
            assert_eq!(
                gitlab_merge_request_comment_event_from_object_attributes_json(attributes)
                    .is_some(),
                read_as_command,
                "{action}"
            );
        }
    }

    #[test]
    fn gitlab_merge_request_note_webhook_parses_created_note_not_updated_note_as_command() {
        let build_approve_note_webhook_body_for_action = |action: &str| {
            serde_json::to_vec(&serde_json::json!({
                "object_attributes": {
                    "id": 1,
                    "note": "/gradient approve",
                    "noteable_type": "MergeRequest",
                    "action": action,
                },
                "merge_request": { "iid": 1 },
                "user": { "username": "maintainer" },
                "project": { "path_with_namespace": "group/proj" },
            }))
            .unwrap()
        };
        assert!(
            parse_comment_event(
                GitHostType::GitLab,
                &build_approve_note_webhook_body_for_action("update")
            )
            .is_none()
        );
        let created_note_comment_event = parse_comment_event(
            GitHostType::GitLab,
            &build_approve_note_webhook_body_for_action("create"),
        );
        assert_eq!(
            created_note_comment_event.unwrap().comment_body,
            "/gradient approve"
        );
    }

    #[test]
    fn parse_comment_event_gitlab_ignores_non_merge_request_note() {
        let body = serde_json::to_vec(&serde_json::json!({
            "object_attributes": {
                "id": 1,
                "note": "/gradient run",
                "noteable_type": "Issue",
            },
        }))
        .unwrap();
        assert!(parse_comment_event(GitHostType::GitLab, &body).is_none());
    }

    fn utc_date_time_2026_11_19(hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 19)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
    }

    fn parse_comment_event_with_created_at(
        git_host: GitHostType,
        created_at: serde_json::Value,
    ) -> CommentEvent {
        event_of(
            git_host,
            serde_json::json!({
                "action": "created",
                "comment": { "id": 1, "body": "/gradient approve", "created_at": created_at },
                "pull_request": { "number": 1 },
                "sender": { "login": "maintainer" },
                "repository": { "full_name": "acme/widgets" },
            }),
        )
    }

    fn parse_merge_request_note_comment_event_with_created_at(
        created_at: serde_json::Value,
    ) -> CommentEvent {
        event_of(
            GitHostType::GitLab,
            serde_json::json!({
                "object_attributes": {
                    "id": 1,
                    "note": "/gradient approve",
                    "noteable_type": "MergeRequest",
                    "created_at": created_at,
                },
                "merge_request": { "iid": 1 },
                "user": { "username": "maintainer" },
                "project": { "path_with_namespace": "group/proj" },
            }),
        )
    }

    #[test]
    fn test_parses_comment_creation_time_from_github_gitea_and_forgejo_payloads() {
        let expected = Some(utc_date_time_2026_11_19(10, 15, 30));
        for (git_host, created_at) in [
            (GitHostType::GitHub, "2026-11-19T10:15:30Z"),
            (GitHostType::Gitea, "2026-11-19T12:15:30+02:00"),
            (GitHostType::Forgejo, "2026-11-19T05:15:30-05:00"),
        ] {
            let event = parse_comment_event_with_created_at(git_host, created_at.into());
            assert_eq!(event.comment_created_at, expected, "{created_at}");
        }
    }

    #[test]
    fn gitlab_merge_request_note_creation_time_parses_both_formats_as_utc() {
        let older_form = parse_merge_request_note_comment_event_with_created_at(
            "2026-11-19 10:15:30 UTC".into(),
        );
        assert_eq!(
            older_form.comment_created_at,
            Some(utc_date_time_2026_11_19(10, 15, 30))
        );
        let rfc_3339 = parse_merge_request_note_comment_event_with_created_at(
            "2026-11-19T11:15:30.123+01:00".into(),
        );
        assert_eq!(
            rfc_3339.comment_created_at,
            Some(utc_date_time_2026_11_19(10, 15, 30) + chrono::Duration::milliseconds(123))
        );
    }

    fn approval_decision_for_bare_approve_with_recorded_head_commit_time(
        event: &CommentEvent,
        head_recorded_at: chrono::NaiveDateTime,
    ) -> MaintainerCommentCommandDecision {
        let command = parse_gradient_command(&event.comment_body).expect("approve command");
        let comment_time = comment_time_used_for_approval_decision(
            event.comment_created_at,
            fixed_test_comment_time() + chrono::Duration::seconds(5),
        );
        decide_allowed_comment_action(
            &command,
            &COMMIT_Y,
            &[pending_approval_for_commit_recorded_at(
                COMMIT_Y,
                head_recorded_at,
            )],
            comment_time,
        )
    }

    fn assert_parsed_comment_approve_accepts_head_recorded_ten_minutes_before_and_rejects_head_recorded_fifty_nine_seconds_before(
        event: &CommentEvent,
    ) {
        assert_eq!(event.comment_created_at, Some(fixed_test_comment_time()));
        assert_eq!(
            approval_decision_for_bare_approve_with_recorded_head_commit_time(
                event,
                utc_date_time_2026_11_19(10, 5, 0),
            ),
            expected_decision_releasing_evaluations_waiting_for_commit_approval(COMMIT_Y)
        );
        let recorded_59_seconds_before = utc_date_time_2026_11_19(10, 14, 1);
        let refusal = approval_decision_for_bare_approve_with_recorded_head_commit_time(
            event,
            recorded_59_seconds_before,
        );
        assert_eq!(
            refusal,
            expected_refusal_decision_for_head_recorded_less_than_60_seconds_before_comment(
                recorded_59_seconds_before
            )
        );
        let MaintainerCommentCommandDecision::RefuseBareApproveBecauseHeadCommitWasRecordedLessThanSixtySecondsBeforeComment {
            pull_request_head_commit_hash,
            head_commit_recorded_at,
        } = refusal
        else {
            unreachable!();
        };
        let reply = build_bare_approve_reply_comment_when_head_commit_recorded_too_recently(
            &pull_request_head_commit_hash,
            head_commit_recorded_at,
        );
        assert!(reply.contains(&format!("/gradient approve {}", hex::encode(COMMIT_Y))));
        assert!(reply.contains("2026-11-19 10:14:01 UTC"));
    }

    #[test]
    fn gitlab_comment_creation_time_in_old_format_and_rfc3339_is_compared_with_head_recorded_time_for_bare_approve()
     {
        for created_at in ["2026-11-19 10:15:00 UTC", "2026-11-19T10:15:00Z"] {
            let event = parse_merge_request_note_comment_event_with_created_at(created_at.into());
            assert_parsed_comment_approve_accepts_head_recorded_ten_minutes_before_and_rejects_head_recorded_fifty_nine_seconds_before(
                &event,
            );
        }
    }

    #[test]
    fn github_app_comment_webhook_creation_time_decides_bare_approve_against_head_recorded_time() {
        let event = event_of(
            GitHostType::GitHub,
            serde_json::json!({
                "action": "created",
                "installation": { "id": 4242 },
                "issue": {
                    "number": 7,
                    "pull_request": { "url": "https://api.github.com/repos/acme/widgets/pulls/7" },
                },
                "comment": {
                    "id": 1,
                    "body": "/gradient approve",
                    "created_at": "2026-11-19T10:15:00Z",
                    "updated_at": "2026-11-19T10:15:00Z",
                },
                "sender": { "login": "maintainer" },
                "repository": { "full_name": "acme/widgets" },
            }),
        );
        assert_eq!(event.pr_number, Some(7));
        assert_parsed_comment_approve_accepts_head_recorded_ten_minutes_before_and_rejects_head_recorded_fifty_nine_seconds_before(
            &event,
        );
    }

    #[test]
    fn comment_payload_with_missing_or_invalid_creation_time_has_absent_creation_time() {
        for created_at in [
            serde_json::Value::Null,
            "yesterday".into(),
            "2026-11-19 10:15:30".into(),
            "2026-11-19 10:15:30 +0200".into(),
        ] {
            assert_eq!(
                parse_comment_event_with_created_at(GitHostType::GitHub, created_at.clone())
                    .comment_created_at,
                None
            );
            assert_eq!(
                parse_merge_request_note_comment_event_with_created_at(created_at)
                    .comment_created_at,
                None
            );
        }
        let without_field = event_of(
            GitHostType::Gitea,
            serde_json::json!({
                "action": "created",
                "comment": { "id": 1, "body": "/gradient approve" },
                "pull_request": { "number": 1 },
            }),
        );
        assert_eq!(without_field.comment_created_at, None);
    }

    #[test]
    fn parse_gradient_run_bare_returns_run_without_wildcard() {
        assert_eq!(
            parse_gradient_command("/gradient run"),
            Some(GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: None
            })
        );
        assert_eq!(
            parse_gradient_command("   /gradient run   "),
            Some(GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: None
            })
        );
        assert_eq!(
            parse_gradient_command("\n/gradient run\n"),
            Some(GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: None
            })
        );
    }

    #[test]
    fn parse_gradient_run_is_case_insensitive() {
        assert_eq!(
            parse_gradient_command("/GRADIENT Run"),
            Some(GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: None
            })
        );
        assert_eq!(
            parse_gradient_command("/Gradient RUN packages.*.*"),
            Some(GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: Some("packages.*.*".to_string())
            })
        );
    }

    #[test]
    fn parse_gradient_run_with_complex_wildcard_preserves_raw() {
        let body = "/gradient run packages.*.foo,!packages.x86_64-linux.broken";
        let Some(GradientCommand::Run {
            wildcard: Some(w), ..
        }) = parse_gradient_command(body)
        else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.foo,!packages.x86_64-linux.broken");
    }

    #[test]
    fn parse_gradient_run_trims_whitespace_around_wildcard() {
        let body = "   /gradient run   packages.*.*   ";
        let Some(GradientCommand::Run {
            wildcard: Some(w), ..
        }) = parse_gradient_command(body)
        else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_gradient_run_after_quote_reply() {
        let body = "> @maintainer asked us to retrigger\n> after rebasing main\n\n/gradient run packages.*.*";
        let Some(GradientCommand::Run {
            wildcard: Some(w), ..
        }) = parse_gradient_command(body)
        else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_gradient_approve() {
        assert_eq!(
            parse_gradient_command("/gradient approve"),
            Some(GradientCommand::Approve {
                commit_named_in_comment: NoCommitHashSpecified
            })
        );
        assert_eq!(
            parse_gradient_command("   /GRADIENT Approve   "),
            Some(GradientCommand::Approve {
                commit_named_in_comment: NoCommitHashSpecified
            })
        );
    }

    #[test]
    fn parse_gradient_approve_rejects_trailing_args() {
        assert!(parse_gradient_command("/gradient approve packages.*").is_none());
    }

    #[test]
    fn parse_gradient_summary_takes_no_arguments() {
        assert_eq!(
            parse_gradient_command("/gradient Summary"),
            Some(GradientCommand::Summary)
        );
        assert!(parse_gradient_command("/gradient summary packages.*").is_none());
    }

    #[test]
    fn parse_gradient_abort_takes_no_arguments() {
        assert_eq!(
            parse_gradient_command("/gradient ABORT"),
            Some(GradientCommand::Abort)
        );
        assert!(parse_gradient_command("/gradient abort packages.*").is_none());
    }

    #[test]
    fn parse_gradient_rejects_unknown_subcommand() {
        assert!(parse_gradient_command("/gradient yolo").is_none());
        assert!(parse_gradient_command("/gradient").is_none());
    }

    #[test]
    fn parse_gradient_rejects_unrelated() {
        assert!(parse_gradient_command("looks good").is_none());
        assert!(parse_gradient_command("/gradientrun").is_none());
        assert!(parse_gradient_command("foo\n/gradient run\nbar").is_none());
        assert!(
            parse_gradient_command("quote-reply context\n\n/gradient run").is_none(),
            "non-quote prose before /gradient must reject"
        );
    }

    #[test]
    fn non_pusher_gets_no_reply_and_maintainer_gets_wildcard_error_or_command_runs() {
        let parse_error = "packages.** ,<img src=x onerror=alert(1)>"
            .parse::<Wildcard>()
            .unwrap_err();
        assert_eq!(
            comment_command_action_from_push_permission_and_parse_error(false, Some(&parse_error)),
            PullRequestCommentCommandOutcome::CommenterCannotPushIgnoreCommand
        );
        assert_eq!(
            comment_command_action_from_push_permission_and_parse_error(false, None),
            PullRequestCommentCommandOutcome::CommenterCannotPushIgnoreCommand
        );
        assert_eq!(
            comment_command_action_from_push_permission_and_parse_error(true, Some(&parse_error)),
            PullRequestCommentCommandOutcome::MaintainerWildcardParseError(&parse_error)
        );
        assert_eq!(
            comment_command_action_from_push_permission_and_parse_error(true, None),
            PullRequestCommentCommandOutcome::ValidMaintainerCommand
        );
    }

    #[test]
    fn invalid_wildcard_reply_names_violated_rule_without_repeating_commenter_text() {
        let raw = "packages.** ,<img src=x onerror=alert(1)>";
        let parse_error = raw.parse::<Wildcard>().unwrap_err();
        let reply = build_invalid_gradient_run_wildcard_reply_comment(&parse_error);
        assert!(reply.contains(&parse_error.to_string()), "{reply}");
        assert!(!reply.contains("onerror"), "{reply}");
        assert!(!reply.contains(raw), "{reply}");
    }

    #[test]
    fn parse_gradient_legacy_ci_prefix_rejected() {
        assert!(parse_gradient_command("/ci run").is_none());
        assert!(parse_gradient_command("/ci approve").is_none());
    }

    fn commit(word: Option<&str>) -> CommitSpecifiedInCommentCommand {
        word.map_or(NoCommitHashSpecified, |word| {
            parse_commit_from_comment_word(word).unwrap()
        })
    }

    fn approve(word: Option<&str>) -> GradientCommand {
        GradientCommand::Approve {
            commit_named_in_comment: commit(word),
        }
    }

    fn run(word: Option<&str>) -> GradientCommand {
        GradientCommand::Run {
            commit_named_in_comment: commit(word),
            wildcard: None,
        }
    }

    const COMMIT_X: [u8; 20] = [0xaa; 20];
    const COMMIT_Y: [u8; 20] = [0xbb; 20];

    fn pending_approval_for_commit_recorded_at(
        commit: [u8; 20],
        recorded_at: chrono::NaiveDateTime,
    ) -> PendingForkPullRequestApprovalCommitAndGradientRecordedAt {
        PendingForkPullRequestApprovalCommitAndGradientRecordedAt {
            commit_hash: commit.to_vec(),
            waiting_commit_recorded_at: recorded_at,
        }
    }

    fn fixed_test_comment_time() -> chrono::NaiveDateTime {
        utc_date_time_2026_11_19(10, 15, 0)
    }

    fn expected_decision_releasing_evaluations_waiting_for_commit_approval(
        commit: [u8; 20],
    ) -> MaintainerCommentCommandDecision {
        MaintainerCommentCommandDecision::ReleaseEvaluationsWaitingForMaintainerApproval(
            commit.to_vec(),
        )
    }

    fn expected_refusal_decision_for_head_recorded_less_than_60_seconds_before_comment(
        recorded_at: chrono::NaiveDateTime,
    ) -> MaintainerCommentCommandDecision {
        MaintainerCommentCommandDecision::RefuseBareApproveBecauseHeadCommitWasRecordedLessThanSixtySecondsBeforeComment {
            pull_request_head_commit_hash: COMMIT_Y.to_vec(),
            head_commit_recorded_at: recorded_at,
        }
    }

    #[test]
    fn test_approve_without_commit_approves_pull_request_head_recorded_ten_minutes_before_and_sixty_seconds_before_comment()
     {
        for recorded_at in [
            utc_date_time_2026_11_19(10, 5, 0),
            utc_date_time_2026_11_19(10, 14, 0),
        ] {
            let pending = [
                pending_approval_for_commit_recorded_at(
                    COMMIT_X,
                    utc_date_time_2026_11_19(9, 0, 0),
                ),
                pending_approval_for_commit_recorded_at(COMMIT_Y, recorded_at),
            ];
            assert_eq!(
                decide_allowed_comment_action(
                    &approve(None),
                    &COMMIT_Y,
                    &pending,
                    Some(fixed_test_comment_time()),
                ),
                expected_decision_releasing_evaluations_waiting_for_commit_approval(COMMIT_Y),
                "{recorded_at}"
            );
        }
    }

    #[test]
    fn approve_without_commit_is_refused_and_names_head_and_recorded_time_when_head_was_recorded_59_seconds_before_or_after_comment()
     {
        for recorded_at in [
            utc_date_time_2026_11_19(10, 14, 1),
            utc_date_time_2026_11_19(10, 15, 0),
            utc_date_time_2026_11_19(10, 16, 0),
        ] {
            let pending = [pending_approval_for_commit_recorded_at(
                COMMIT_Y,
                recorded_at,
            )];
            assert_eq!(
                decide_allowed_comment_action(
                    &approve(None),
                    &COMMIT_Y,
                    &pending,
                    Some(fixed_test_comment_time()),
                ),
                expected_refusal_decision_for_head_recorded_less_than_60_seconds_before_comment(
                    recorded_at
                ),
                "{recorded_at}"
            );
        }
    }

    #[test]
    fn approve_without_commit_is_refused_when_comment_time_is_missing_or_head_has_no_pending_approval()
     {
        let refuse = MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit;
        let long_ago = utc_date_time_2026_11_19(9, 0, 0);
        let head_pending = [pending_approval_for_commit_recorded_at(COMMIT_Y, long_ago)];
        assert_eq!(
            decide_allowed_comment_action(&approve(None), &COMMIT_Y, &head_pending, None),
            refuse
        );
        let older_commit_pending = [pending_approval_for_commit_recorded_at(COMMIT_X, long_ago)];
        assert_eq!(
            decide_allowed_comment_action(
                &approve(None),
                &COMMIT_Y,
                &older_commit_pending,
                Some(fixed_test_comment_time()),
            ),
            refuse
        );
        assert_eq!(
            decide_allowed_comment_action(
                &approve(None),
                &COMMIT_Y,
                &[],
                Some(fixed_test_comment_time()),
            ),
            MaintainerCommentCommandDecision::NoEvaluationWaitsForApproval
        );
    }

    #[test]
    fn future_comment_created_at_is_capped_and_head_recorded_before_receipt_is_refused() {
        let received_at = fixed_test_comment_time();
        let created_at_one_hour_ahead = utc_date_time_2026_11_19(11, 15, 0);
        let capped =
            comment_time_used_for_approval_decision(Some(created_at_one_hour_ahead), received_at);
        assert_eq!(capped, Some(received_at));
        let recorded_at = utc_date_time_2026_11_19(10, 14, 30);
        let pending = [pending_approval_for_commit_recorded_at(
            COMMIT_Y,
            recorded_at,
        )];
        assert_eq!(
            decide_allowed_comment_action(&approve(None), &COMMIT_Y, &pending, capped),
            expected_refusal_decision_for_head_recorded_less_than_60_seconds_before_comment(
                recorded_at
            )
        );
        assert_eq!(
            comment_time_used_for_approval_decision(None, received_at),
            None
        );
    }

    #[test]
    fn comment_naming_full_hash_releases_only_that_commit_pending_approval_and_approve_without_pending_approval_is_refused()
     {
        let pending_approvals_for_two_commits_at_comment_time = [
            pending_approval_for_commit_recorded_at(COMMIT_X, fixed_test_comment_time()),
            pending_approval_for_commit_recorded_at(COMMIT_Y, fixed_test_comment_time()),
        ];
        let full_x = hex::encode(COMMIT_X);
        for cmd in [approve(Some(&full_x)), run(Some(&full_x))] {
            assert_eq!(
                decide_allowed_comment_action(
                    &cmd,
                    &COMMIT_Y,
                    &pending_approvals_for_two_commits_at_comment_time,
                    Some(fixed_test_comment_time()),
                ),
                expected_decision_releasing_evaluations_waiting_for_commit_approval(COMMIT_X),
                "{cmd:?}"
            );
        }
        assert_eq!(
            decide_allowed_comment_action(
                &approve(Some(&hex::encode(COMMIT_Y))),
                &COMMIT_Y,
                &pending_approvals_for_two_commits_at_comment_time[..1],
                Some(fixed_test_comment_time()),
            ),
            MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit
        );
    }

    #[test]
    fn run_without_commit_starts_head_and_run_with_full_hash_only_starts_if_hash_is_head() {
        let refuse = MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit;
        let start = MaintainerCommentCommandDecision::StartPullRequestHeadCommitEvaluation;
        let cases = [
            (run(None), start.clone()),
            (run(Some(&hex::encode(COMMIT_X))), refuse),
            (run(Some(&hex::encode(COMMIT_Y))), start),
        ];
        for (cmd, expected) in cases {
            assert_eq!(
                decide_allowed_comment_action(&cmd, &COMMIT_Y, &[], None),
                expected,
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn short_commit_prefix_does_not_release_evaluation_waiting_for_approval() {
        let pending = [pending_approval_for_commit_recorded_at(
            COMMIT_Y,
            utc_date_time_2026_11_19(9, 0, 0),
        )];
        for cmd in [approve(Some("bbbbbbb")), run(Some("bbbbbbb"))] {
            assert_eq!(
                decide_allowed_comment_action(
                    &cmd,
                    &COMMIT_Y,
                    &pending,
                    Some(fixed_test_comment_time()),
                ),
                MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit,
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn gradient_run_with_full_commit_hash_that_differs_from_head_but_shares_seven_character_prefix_on_pull_request_without_earlier_evaluation_is_refused_and_starts_nothing()
     {
        let mut head = COMMIT_X;
        head[19] = 0x01;
        let full_x = hex::encode(COMMIT_X);
        for cmd in [run(Some(&full_x)), run(Some(&full_x[..7]))] {
            assert_eq!(
                decide_allowed_comment_action(&cmd, &head, &[], None),
                MaintainerCommentCommandDecision::RefuseAndReplyWithCommandNamingHeadCommit,
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn test_reads_approve_and_run_comment_commands_accepts_any_case_full_40_hex_commit_hash_marks_7_to_39_hex_word_as_abbreviated_hash_to_refuse_and_keeps_other_words_wildcard_for_run_invalid_for_approve()
     {
        assert_eq!(
            parse_gradient_command("/gradient approve ABCDEF0"),
            Some(GradientCommand::Approve {
                commit_named_in_comment: AbbreviatedCommitHash
            })
        );
        assert_eq!(parse_gradient_command("/gradient approve abcdef"), None);
        assert_eq!(parse_gradient_command("/gradient approve abcdef0 x"), None);
        assert_eq!(
            parse_gradient_command(&format!("/gradient run {} packages.*.*", "A".repeat(40))),
            Some(GradientCommand::Run {
                commit_named_in_comment: FullCommitHash(COMMIT_X.to_vec()),
                wildcard: Some("packages.*.*".to_owned()),
            })
        );
        assert_eq!(
            parse_gradient_command(&format!("/gradient run {}", "a".repeat(39))),
            Some(GradientCommand::Run {
                commit_named_in_comment: AbbreviatedCommitHash,
                wildcard: None,
            })
        );
        assert_eq!(
            parse_gradient_command(&format!("/gradient run {}", "a".repeat(41))),
            Some(GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: Some("a".repeat(41)),
            })
        );
    }

    #[test]
    fn refusal_reply_shows_full_lowercase_head_commit_and_keeps_run_wildcard() {
        let head = "AB".repeat(20);
        let reply = build_reply_comment_for_commit_not_matching_head(
            &GradientCommand::Run {
                commit_named_in_comment: NoCommitHashSpecified,
                wildcard: Some("packages.*.*".to_owned()),
            },
            &head,
        );
        assert!(
            reply.contains(&format!("/gradient run {} packages.*.*", "ab".repeat(20))),
            "{reply}"
        );
        let reply = build_reply_comment_for_commit_not_matching_head(&approve(None), &head);
        assert!(
            reply.contains(&format!("/gradient approve {}", "ab".repeat(20))),
            "{reply}"
        );
    }

    #[test]
    fn bare_approve_too_recent_refusal_contains_full_hash_utc_recorded_time_and_retry_line() {
        let reply = build_bare_approve_reply_comment_when_head_commit_recorded_too_recently(
            &COMMIT_Y,
            utc_date_time_2026_11_19(10, 14, 1),
        );
        let full_head_hash = "bb".repeat(20);
        assert!(reply.contains(&full_head_hash), "{reply}");
        assert!(reply.contains("2026-11-19 10:14:01 UTC"), "{reply}");
        assert!(reply.contains("60 seconds"), "{reply}");
        assert!(
            reply.contains(&format!("\n/gradient approve {full_head_hash}\n")),
            "{reply}"
        );
    }
}
