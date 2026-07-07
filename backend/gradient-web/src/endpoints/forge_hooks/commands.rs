/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `/gradient` PR-comment command dispatch (Gitea/Forgejo/GitLab + GitHub App).

use super::approval::{
    PullRequestApprovalContext, dispatch_approval_granted, sender_is_trusted,
    submit_pr_approval_review,
};
use super::fanout::trigger_pr_for_integration;
use super::installation::resolve_github_app_targets;
use super::payloads::CommentPayload;
use gradient_ci::{
    find_approval_gated_eval, set_evaluation_source_comment, unpark_approval,
    unpark_approval_with_wildcard,
};
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::triggers::TriggerType;
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use sea_orm::{DbBackend, FromQueryResult, Statement, Value};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// The fields `handle_issue_comment` consumes, normalized across the GitHub /
/// Gitea comment payload and the GitLab Note Hook payload.
struct CommentEvent {
    comment_body: String,
    pr_number: Option<u64>,
    sender_login: Option<String>,
    owner_repo: Option<String>,
    comment_id: Option<i64>,
}

fn parse_comment_event(forge: ForgeType, body: &[u8]) -> Option<CommentEvent> {
    let payload: CommentPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "comment webhook: failed to parse payload");
            return None;
        }
    };

    match forge {
        ForgeType::GitLab => {
            let attrs = payload.object_attributes?;
            if attrs.noteable_type.as_deref() != Some("MergeRequest") {
                return None;
            }
            Some(CommentEvent {
                comment_id: attrs.id,
                comment_body: attrs.note.unwrap_or_default(),
                pr_number: payload.merge_request.and_then(|m| m.iid),
                sender_login: payload.user.and_then(|u| u.username.or(u.login)),
                owner_repo: payload.project.and_then(|p| p.path_with_namespace),
            })
        }
        _ => {
            // GitHub and Gitea both use `action == "created"`.
            if payload.action.as_deref() != Some("created") {
                return None;
            }
            let comment = payload.comment.unwrap_or_default();
            Some(CommentEvent {
                comment_id: comment.id,
                comment_body: comment.body.unwrap_or_default(),
                pr_number: payload
                    .pull_request
                    .or(payload.issue)
                    .and_then(|i| i.number),
                sender_login: payload.sender.and_then(|s| s.login.or(s.username)),
                owner_repo: payload.repository.and_then(|r| r.full_name),
            })
        }
    }
}

/// Handle a `/gradient run [wildcard]` or `/gradient approve` comment on a PR.
/// Both commands are maintainer-only. `integration_id` is `Some` for the
/// per-integration routes and `None` for the shared GitHub App route (where the
/// integration is resolved from `installation.id`).
pub(super) async fn handle_issue_comment(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    forge: ForgeType,
    integration_id: Option<IntegrationId>,
    body: &[u8],
    client_ip: std::net::IpAddr,
) {
    let Some(event) = parse_comment_event(forge, body) else {
        return;
    };
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

    let Some(integration_ids) =
        resolve_comment_integrations(state, integration_id, body, &owner_repo, client_ip).await
    else {
        return;
    };

    let wildcard_override = match resolve_wildcard_override(
        state,
        &cmd,
        &integration_ids,
        owner,
        repo,
        pr_number,
    )
    .await
    {
        Ok(w) => w,
        Err(()) => return,
    };

    let reaction_target = event.comment_id.map(|id| gradient_ci::ReactionTarget {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr_number,
        comment_id: id,
    });

    let mut action_taken = false;
    for integration_id in &integration_ids {
        let dispatch = CommentDispatch {
            state,
            scheduler,
            integration_id: *integration_id,
            cmd: &cmd,
            owner,
            repo,
            sender: &sender,
            pr_number,
            wildcard_override: &wildcard_override,
            reaction_target: &reaction_target,
        };
        if handle_comment_for_integration(&dispatch).await {
            action_taken = true;
        }
    }
    if !action_taken {
        debug!(
            pr_number,
            "/gradient comment had no parked evaluation and produced no fresh fire"
        );
    }
}

/// Resolve the inbound integrations addressed by this comment: the route's own
/// integration, or (GitHub App route) those bound to the payload's installation.
async fn resolve_comment_integrations(
    state: &Arc<ServerState>,
    integration_id: Option<IntegrationId>,
    body: &[u8],
    owner_repo: &str,
    client_ip: std::net::IpAddr,
) -> Option<Vec<IntegrationId>> {
    if let Some(id) = integration_id {
        return Some(vec![id]);
    }
    let Some(installation_id) = github_installation_id_from_comment_body(body) else {
        warn!("comment webhook (github): no installation_id");
        return None;
    };
    let repo_urls = vec![
        format!("https://github.com/{owner_repo}"),
        format!("https://github.com/{owner_repo}.git"),
        format!("git@github.com:{owner_repo}.git"),
    ];
    let targets = resolve_github_app_targets(state, installation_id, &repo_urls, client_ip).await;
    if targets.is_empty() {
        warn!(installation_id, %owner_repo, "comment webhook (github): no integration matched");
        return None;
    }
    Some(targets)
}

/// Validate a `/gradient run <wildcard>` override. `Err(())` means the wildcard
/// was rejected and an error comment already posted, so the caller must abort.
async fn resolve_wildcard_override(
    state: &Arc<ServerState>,
    cmd: &GradientCommand,
    integration_ids: &[IntegrationId],
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Option<String>, ()> {
    let GradientCommand::Run {
        wildcard: Some(raw),
    } = cmd
    else {
        return Ok(None);
    };
    match raw.parse::<Wildcard>() {
        Ok(_) => Ok(Some(raw.clone())),
        Err(e) => {
            warn!(wildcard = %raw, error = %e, "/gradient run wildcard rejected");
            post_wildcard_error_comment(state, integration_ids, owner, repo, pr_number, raw, &e)
                .await;
            Err(())
        }
    }
}

/// Per-integration inputs for a `/gradient` comment dispatch.
struct CommentDispatch<'a> {
    state: &'a Arc<ServerState>,
    scheduler: &'a Arc<Scheduler>,
    integration_id: IntegrationId,
    cmd: &'a GradientCommand,
    owner: &'a str,
    repo: &'a str,
    sender: &'a str,
    pr_number: u64,
    wildcard_override: &'a Option<String>,
    reaction_target: &'a Option<gradient_ci::ReactionTarget>,
}

/// Run the maintainer trust probe once for the integration, then try the
/// unpark path and fall back to a fresh `/gradient run`. Returns whether any
/// action fired.
async fn handle_comment_for_integration(ctx: &CommentDispatch<'_>) -> bool {
    let project_ids = match active_project_ids_for_integration(ctx.state, ctx.integration_id).await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "comment webhook: failed to load project list");
            return false;
        }
    };
    let Some(probe_project) = first_project_with_reporter(ctx.state, &project_ids).await else {
        warn!(
            integration_id = %ctx.integration_id,
            pr_number = ctx.pr_number,
            projects = project_ids.len(),
            "/gradient comment ignored: integration has no project with a usable forge reporter (needs an API token)"
        );
        return false;
    };
    let is_maintainer =
        sender_is_trusted(ctx.state, probe_project, ctx.owner, ctx.repo, ctx.sender).await;
    if let Some(target) = ctx.reaction_target {
        let kind = if is_maintainer {
            gradient_ci::ReactionKind::Eyes
        } else {
            gradient_ci::ReactionKind::Confused
        };
        fire_reaction_via_project(ctx.state, probe_project, target, kind).await;
    }
    if !is_maintainer {
        warn!(
            integration_id = %ctx.integration_id,
            pr_number = ctx.pr_number,
            sender = %ctx.sender,
            "Rejecting /gradient command - sender is not a repo writer"
        );
        return false;
    }

    if unpark_existing_approvals(ctx, &project_ids).await {
        return true;
    }
    if !matches!(ctx.cmd, GradientCommand::Run { .. }) {
        return false;
    }
    run_fresh_evaluation(ctx, &project_ids).await
}

/// Unpark any approval-gated evaluation already parked for this PR. Returns
/// whether at least one eval was released.
async fn unpark_existing_approvals(ctx: &CommentDispatch<'_>, project_ids: &[ProjectId]) -> bool {
    let mut unparked_any = false;
    for project_id in project_ids {
        let Ok(Some(eval)) =
            find_approval_gated_eval(&ctx.state.web_db, *project_id, ctx.pr_number).await
        else {
            continue;
        };
        let unpark_result = match ctx.wildcard_override {
            None => unpark_approval(&ctx.state.web_db, eval.id).await,
            Some(w) => unpark_approval_with_wildcard(&ctx.state.web_db, eval.id, w).await,
        };
        match unpark_result {
            Ok(Some(unparked)) => {
                info!(
                    evaluation_id = %eval.id,
                    pr_number = ctx.pr_number,
                    sender = %ctx.sender,
                    wildcard_override = ctx.wildcard_override.as_deref(),
                    "PR approval gate cleared via /gradient comment"
                );
                if let Some(target) = ctx.reaction_target {
                    let json = serde_json::json!({
                        "owner": target.owner,
                        "repo": target.repo,
                        "pr_number": target.pr_number,
                        "comment_id": target.comment_id,
                    });
                    if let Err(e) =
                        set_evaluation_source_comment(&ctx.state.web_db, unparked.id, json).await
                    {
                        warn!(error = %e, evaluation_id = %unparked.id, "stamp source_comment on unparked eval failed");
                    }
                }
                dispatch_approval_granted(ctx.state, &unparked).await;
                if matches!(ctx.cmd, GradientCommand::Approve) {
                    submit_pr_approval_review(
                        ctx.state,
                        *project_id,
                        ctx.owner,
                        ctx.repo,
                        ctx.pr_number,
                    )
                    .await;
                }
                unparked_any = true;
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate")
            }
        }
    }
    unparked_any
}

/// Fetch the PR head and fire a fresh evaluation. The maintainer is already
/// trust-verified, so `sender` lets `decide_pr_gate` treat the command itself as
/// the approval, even on a fork PR. Returns whether the fan-out queued anything.
async fn run_fresh_evaluation(ctx: &CommentDispatch<'_>, project_ids: &[ProjectId]) -> bool {
    let Some(snapshot) =
        fetch_pr_snapshot(ctx.state, project_ids, ctx.owner, ctx.repo, ctx.pr_number).await
    else {
        warn!(
            integration_id = %ctx.integration_id,
            pr_number = ctx.pr_number,
            "/gradient run: could not fetch PR head via any project reporter"
        );
        return false;
    };
    let commit_hash = match hex::decode(&snapshot.head_sha) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, sha = %snapshot.head_sha, "/gradient run: invalid head SHA");
            return false;
        }
    };
    let approval_ctx = PullRequestApprovalContext {
        pr_number: Some(ctx.pr_number),
        pr_author: None,
        is_fork: Some(snapshot.is_fork),
        sender: Some(ctx.sender.to_string()),
    };
    let source_comment_json = ctx.reaction_target.as_ref().map(|t| {
        serde_json::json!({
            "owner": t.owner,
            "repo": t.repo,
            "pr_number": t.pr_number,
            "comment_id": t.comment_id,
        })
    });
    let event_repo_urls = [format!("https://github.com/{}/{}", ctx.owner, ctx.repo)];
    let outcome = trigger_pr_for_integration(
        ctx.state,
        ctx.scheduler,
        ctx.integration_id,
        &event_repo_urls,
        Some(snapshot.head_branch.as_str()),
        "synchronize",
        commit_hash,
        None,
        Some(ctx.sender.to_string()),
        approval_ctx,
        snapshot.head_clone_url.clone(),
        true,
        ctx.wildcard_override.clone(),
        source_comment_json,
    )
    .await;
    if !outcome.queued.is_empty() {
        info!(
            integration_id = %ctx.integration_id,
            pr_number = ctx.pr_number,
            sender = %ctx.sender,
            wildcard_override = ctx.wildcard_override.as_deref(),
            queued = outcome.queued.len(),
            "/gradient run created fresh evaluation"
        );
        true
    } else {
        debug!(
            integration_id = %ctx.integration_id,
            pr_number = ctx.pr_number,
            "/gradient run: trigger fan-out queued nothing"
        );
        false
    }
}

/// Post a reaction on a PR/MR comment via the given project's reporter.
/// Best-effort: failures are logged and swallowed.
async fn fire_reaction_via_project(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    target: &gradient_ci::ReactionTarget,
    kind: gradient_ci::ReactionKind,
) {
    let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %project_id, "/gradient reaction: resolving reporter");
            return;
        }
    };
    if let Err(e) = reporter.add_reaction(target, kind).await {
        warn!(error = %e, %project_id, ?kind, "/gradient reaction: post failed");
    }
}

pub(super) async fn first_project_with_reporter(
    state: &Arc<ServerState>,
    project_ids: &[ProjectId],
) -> Option<ProjectId> {
    for project_id in project_ids {
        if let Ok(Some(_)) =
            gradient_ci::actions::reporter_for_project(&state.ci(), *project_id).await
        {
            return Some(*project_id);
        }
    }
    None
}

/// Fetch a [`gradient_ci::PullRequestSnapshot`] using the first project whose
/// reporter resolves. `None` on no reporter, an error, or a missing PR.
async fn fetch_pr_snapshot(
    state: &Arc<ServerState>,
    project_ids: &[ProjectId],
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Option<gradient_ci::PullRequestSnapshot> {
    for project_id in project_ids {
        let reporter =
            match gradient_ci::actions::reporter_for_project(&state.ci(), *project_id).await {
                Ok(Some(r)) => r,
                _ => continue,
            };
        match reporter.get_pull_request(owner, repo, pr_number).await {
            Ok(Some(snap)) => return Some(snap),
            Ok(None) => return None,
            Err(e) => {
                warn!(error = %e, %project_id, "/gradient run: PR fetch failed, trying next project");
            }
        }
    }
    None
}

/// Reply to the PR explaining a `/gradient run <wildcard>` parse failure, via
/// the first project with a usable reporter. Best-effort.
async fn post_wildcard_error_comment(
    state: &Arc<ServerState>,
    integration_ids: &[IntegrationId],
    owner: &str,
    repo: &str,
    pr_number: u64,
    raw_wildcard: &str,
    parse_error: &gradient_types::input::InputError,
) {
    let body = format!(
        "Could not parse wildcard `{}`: {}",
        raw_wildcard, parse_error
    );

    for integration_id in integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, %integration_id, "/gradient run wildcard: failed to load projects for reply comment");
                continue;
            }
        };
        for project_id in project_ids {
            let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id)
                .await
            {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => {
                    warn!(error = %e, %project_id, "/gradient run wildcard: resolving reporter for reply comment");
                    continue;
                }
            };
            match reporter
                .post_pr_comment(owner, repo, pr_number, &body)
                .await
            {
                Ok(()) => return,
                Err(e) => {
                    warn!(error = %e, %project_id, "/gradient run wildcard: reply comment post failed, trying next project");
                }
            }
        }
    }
}

/// Outcome of parsing a `/gradient …` PR comment.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GradientCommand {
    /// `/gradient run [wildcard]` - unpark an existing approval-gated eval or
    /// create a fresh one; the optional raw `wildcard` overrides the attr path.
    Run { wildcard: Option<String> },
    /// `/gradient approve` - clear the approval gate for this PR (no-op if none).
    Approve,
}

/// Lift a `/gradient <subcommand>` from a PR comment. The command must be on its
/// own line; blank lines and `> …` quote-reply lines are skipped, any other
/// prose disqualifies the comment. Subcommands: `run [wildcard]` and `approve`.
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
        if verb.eq_ignore_ascii_case("run") {
            found = Some(GradientCommand::Run {
                wildcard: (!arg.is_empty()).then(|| arg.to_string()),
            });
            continue;
        }
        if verb.eq_ignore_ascii_case("approve") {
            if !arg.is_empty() {
                return None;
            }
            found = Some(GradientCommand::Approve);
            continue;
        }
        return None;
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

pub(super) async fn active_project_ids_for_integration(
    state: &Arc<ServerState>,
    integration_id: IntegrationId,
) -> Result<Vec<ProjectId>, sea_orm::DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT DISTINCT project FROM project_trigger \
         WHERE active = true \
           AND trigger_type = $1 \
           AND (config->>'integration_id')::uuid = $2"
            .to_string(),
        [
            Value::SmallInt(Some(i16::from(TriggerType::ReporterPullRequest))),
            Value::Uuid(Some(Box::new(integration_id.into_inner()))),
        ],
    );

    #[derive(sea_orm::FromQueryResult)]
    struct ProjectRow {
        project: ProjectId,
    }
    ProjectRow::find_by_statement(stmt)
        .all(&state.web_db)
        .await
        .map(|rows| rows.into_iter().map(|r| r.project).collect())
}

#[cfg(test)]
mod tests {
    use super::{CommentEvent, GradientCommand, parse_comment_event, parse_gradient_command};
    use gradient_types::ForgeType;

    fn event_of(forge: ForgeType, value: serde_json::Value) -> CommentEvent {
        parse_comment_event(forge, &serde_json::to_vec(&value).unwrap())
            .expect("payload should parse into a CommentEvent")
    }

    #[test]
    fn parse_comment_event_github_issue_comment() {
        let e = event_of(
            ForgeType::GitHub,
            serde_json::json!({
                "action": "created",
                "comment": { "id": 555, "body": "/gradient run" },
                "issue": { "number": 42 },
                "sender": { "login": "octocat" },
                "repository": { "full_name": "octo/repo" },
            }),
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
            ForgeType::Gitea,
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
            ForgeType::GitLab,
            serde_json::json!({
                "object_attributes": {
                    "id": 123,
                    "note": "/gradient run",
                    "noteable_type": "MergeRequest",
                },
                "merge_request": { "iid": 5 },
                "user": { "username": "gl-user" },
                "project": { "path_with_namespace": "group/proj" },
            }),
        );
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
        assert!(parse_comment_event(ForgeType::GitHub, &body).is_none());
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
        assert!(parse_comment_event(ForgeType::GitLab, &body).is_none());
    }

    #[test]
    fn parse_gradient_run_bare_returns_run_without_wildcard() {
        assert_eq!(
            parse_gradient_command("/gradient run"),
            Some(GradientCommand::Run { wildcard: None })
        );
        assert_eq!(
            parse_gradient_command("   /gradient run   "),
            Some(GradientCommand::Run { wildcard: None })
        );
        assert_eq!(
            parse_gradient_command("\n/gradient run\n"),
            Some(GradientCommand::Run { wildcard: None })
        );
    }

    #[test]
    fn parse_gradient_run_is_case_insensitive() {
        assert_eq!(
            parse_gradient_command("/GRADIENT Run"),
            Some(GradientCommand::Run { wildcard: None })
        );
        assert_eq!(
            parse_gradient_command("/Gradient RUN packages.*.*"),
            Some(GradientCommand::Run {
                wildcard: Some("packages.*.*".to_string())
            })
        );
    }

    #[test]
    fn parse_gradient_run_with_wildcard() {
        assert_eq!(
            parse_gradient_command("/gradient run packages.*.*"),
            Some(GradientCommand::Run {
                wildcard: Some("packages.*.*".to_string())
            })
        );
    }

    #[test]
    fn parse_gradient_run_with_complex_wildcard_preserves_raw() {
        let body = "/gradient run packages.*.foo,!packages.x86_64-linux.broken";
        let Some(GradientCommand::Run { wildcard: Some(w) }) = parse_gradient_command(body) else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.foo,!packages.x86_64-linux.broken");
    }

    #[test]
    fn parse_gradient_run_trims_whitespace_around_wildcard() {
        let body = "   /gradient run   packages.*.*   ";
        let Some(GradientCommand::Run { wildcard: Some(w) }) = parse_gradient_command(body) else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_gradient_run_after_quote_reply() {
        let body = "> @maintainer asked us to retrigger\n> after rebasing main\n\n/gradient run packages.*.*";
        let Some(GradientCommand::Run { wildcard: Some(w) }) = parse_gradient_command(body) else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_gradient_approve() {
        assert_eq!(
            parse_gradient_command("/gradient approve"),
            Some(GradientCommand::Approve)
        );
        assert_eq!(
            parse_gradient_command("   /GRADIENT Approve   "),
            Some(GradientCommand::Approve)
        );
    }

    #[test]
    fn parse_gradient_approve_rejects_trailing_args() {
        assert!(parse_gradient_command("/gradient approve packages.*").is_none());
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
    fn parse_gradient_legacy_ci_prefix_rejected() {
        assert!(parse_gradient_command("/ci run").is_none());
        assert!(parse_gradient_command("/ci approve").is_none());
    }
}
