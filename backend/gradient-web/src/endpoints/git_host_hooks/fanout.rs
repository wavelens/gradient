/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::approval::{PullRequestApprovalContext, sender_is_trusted};
use super::installation::event_repo_matches_task;
use super::response::{QueuedEvaluation, SkippedTask, WebhookTriggerOutcome};
use gradient_ci::CiContext;
use gradient_ci::{ApplyInput, ApplyOutcome, ApprovalInfo, apply_trigger, parse_owner_repo};
use gradient_db::lookup::by_id;
use gradient_db::{DbContext, WebDb};
use gradient_entity::task_trigger as ept;
use gradient_git_host::ParsedMergeGroupEvent;
use gradient_scheduler::Scheduler;
use gradient_types::branch_key;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy)]
pub(super) enum PushRefKind<'a> {
    Branch(&'a str),
    Tag(&'a str),
}

impl PushRefKind<'_> {
    fn branch_key(self) -> String {
        match self {
            Self::Branch(name) => branch_key::branch(name),
            Self::Tag(name) => branch_key::tag(name),
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_push_for_integration(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    ref_kind: PushRefKind<'_>,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        ci,
        scheduler,
        integration_id,
        event_repo_urls,
        TriggerType::ReporterPush,
        ref_kind.branch_key(),
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPush {
                branches,
                tags,
                releases_only,
                ..
            } => {
                if *releases_only {
                    return FilterResult::Skip;
                }
                let matches = match ref_kind {
                    PushRefKind::Branch(name) => glob_matches(branches, name),
                    PushRefKind::Tag(name) => glob_matches(tags, name),
                };
                if matches {
                    FilterResult::Fire
                } else {
                    FilterResult::SkipFilter
                }
            }
            _ => FilterResult::Skip,
        },
        None,
        None,
        false,
        None,
        None,
    )
    .await
}

/// The queue branch is the branch key, and the push event for that branch dedups against this
/// evaluation. The base branch picks the trigger.
pub(super) async fn trigger_merge_group_for_integration(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    group: &ParsedMergeGroupEvent,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        ci,
        scheduler,
        integration_id,
        &group.repository_urls,
        TriggerType::ReporterPush,
        branch_key::branch(&group.queue_branch),
        group.commit_hash.clone(),
        group.commit_message.clone(),
        group.author_name.clone(),
        |cfg| match cfg {
            TriggerConfig::ReporterPush {
                branches,
                releases_only: false,
                ..
            } if glob_matches(branches, &group.base_branch) => FilterResult::Fire,
            TriggerConfig::ReporterPush {
                releases_only: false,
                ..
            } => FilterResult::SkipFilter,
            _ => FilterResult::Skip,
        },
        None,
        None,
        false,
        None,
        None,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_pr_for_integration(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    branch: Option<&str>,
    action: &str,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    approval_ctx: PullRequestApprovalContext,
    head_repo_clone_url: Option<String>,
    manual: bool,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
) -> WebhookTriggerOutcome {
    let action_owned = action.to_string();
    let branch_owned = branch.map(str::to_string);
    let pull_request_key = approval_ctx.pr_number.map_or_else(
        || branch_key::NO_BRANCH.to_owned(),
        branch_key::pull_request,
    );
    fan_out_triggers(
        ci,
        scheduler,
        integration_id,
        event_repo_urls,
        TriggerType::ReporterPullRequest,
        pull_request_key,
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPullRequest {
                branches,
                actions,
                require_approval,
                ..
            } => {
                if !actions.iter().any(|a| a == &action_owned) {
                    return FilterResult::SkipFilter;
                }
                let matches = match branch_owned.as_deref() {
                    Some(b) => glob_matches(branches, b),
                    None => branches.is_empty(),
                };
                if !matches {
                    return FilterResult::SkipFilter;
                }
                FilterResult::FirePr {
                    require_approval: *require_approval,
                }
            }
            _ => FilterResult::Skip,
        },
        Some(approval_ctx),
        head_repo_clone_url,
        manual,
        wildcard_override,
        source_comment,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_release_for_integration(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    tag: Option<&str>,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        ci,
        scheduler,
        integration_id,
        event_repo_urls,
        TriggerType::ReporterPush,
        tag.map_or_else(|| branch_key::NO_BRANCH.to_owned(), branch_key::tag),
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPush {
                tags,
                releases_only,
                ..
            } => {
                if !releases_only {
                    return FilterResult::Skip;
                }
                let matches = match tag {
                    Some(t) => glob_matches(tags, t),
                    None => tags.is_empty(),
                };
                if matches {
                    FilterResult::Fire
                } else {
                    FilterResult::SkipFilter
                }
            }
            _ => FilterResult::Skip,
        },
        None,
        None,
        false,
        None,
        None,
    )
    .await
}

enum FilterResult {
    Fire,
    FirePr { require_approval: bool },
    SkipFilter,
    Skip,
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
async fn fan_out_triggers<F>(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    trigger_type: TriggerType,
    branch: String,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    filter: F,
    approval_ctx: Option<PullRequestApprovalContext>,
    repository_override: Option<String>,
    manual: bool,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
) -> WebhookTriggerOutcome
where
    F: Fn(&TriggerConfig) -> FilterResult,
{
    let triggers = match gradient_db::triggers::active_of_integration(
        &ci.db.web_db,
        integration_id,
        trigger_type,
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "load triggers for integration");
            return WebhookTriggerOutcome::default();
        }
    };

    let source_comment = source_comment.or_else(|| {
        approval_ctx.as_ref().and_then(|c| {
            c.pr_number
                .map(|n| serde_json::json!({ "pr_number": n, "pr_author": c.pr_author }))
        })
    });

    let mut outcome = WebhookTriggerOutcome::default();
    for trig in triggers {
        let Some(cfg) = parse_trigger_config(&trig) else {
            continue;
        };
        let Some(task) = load_trigger_task(&ci.db.web_db, &trig).await else {
            continue;
        };

        if !event_repo_matches_task(event_repo_urls, &task.repository) {
            continue;
        }

        let filter_result = filter(&cfg);
        if matches!(&filter_result, FilterResult::Skip) {
            continue;
        }

        let project_name = project_name_for(&ci.db.web_db, task.project)
            .await
            .unwrap_or_default();

        let pr_require_approval = match filter_result {
            FilterResult::SkipFilter => {
                push_skipped(&mut outcome, &task, project_name, "filter");
                continue;
            }
            FilterResult::Fire => false,
            FilterResult::FirePr { require_approval } => require_approval,
            FilterResult::Skip => continue,
        };
        outcome.tasks_scanned += 1;

        let gate_approval = if pr_require_approval {
            decide_pr_gate(ci, &task, approval_ctx.as_ref()).await
        } else {
            None
        };

        let input = ApplyInput {
            trigger_id: trig.id,
            trigger_type,
            branch: branch.clone(),
            commit_hash: commit_hash.clone(),
            commit_message: commit_message.clone(),
            author_name: author_name.clone(),
            manual,
            gate_approval,
            repository_override: repository_override.clone(),
            wildcard_override: wildcard_override.clone(),
            source_comment: source_comment.clone(),
            instance_max_storage_gb: ci.db.config.cache.max_storage_gb,
        };

        apply_and_record(
            &ci.db,
            scheduler,
            &task,
            &trig,
            input,
            project_name,
            &mut outcome,
        )
        .await;
    }
    outcome
}

fn parse_trigger_config(trig: &ept::Model) -> Option<TriggerConfig> {
    match TriggerConfig::parse_row(trig.trigger_type, &trig.config) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!(trigger_id = %trig.id, error = %e, "skip trigger with invalid config");
            None
        }
    }
}

async fn load_trigger_task(db: &WebDb, trig: &ept::Model) -> Option<MTask> {
    match by_id::<ETask, _>(db, trig.task).await {
        Ok(Some(p)) => Some(p),
        Ok(None) => {
            warn!(trigger_id = %trig.id, task_id = %trig.task, "task not found for trigger");
            None
        }
        Err(e) => {
            warn!(error = %e, trigger_id = %trig.id, "DB error fetching task for trigger");
            None
        }
    }
}

async fn apply_and_record(
    db: &DbContext,
    scheduler: &Arc<Scheduler>,
    task: &MTask,
    trig: &ept::Model,
    input: ApplyInput,
    project_name: String,
    outcome: &mut WebhookTriggerOutcome,
) {
    let apply_result = apply_trigger(&db.web_db, task, input).await;
    touch_trigger_last_fired(&db.web_db, trig).await;
    match apply_result {
        Ok(ApplyOutcome::Created {
            evaluation: eval,
            aborted_evaluation,
            hard_abort,
        }) => {
            if let Some(aborted_id) = aborted_evaluation {
                scheduler
                    .stop_aborted_evaluation(aborted_id, hard_abort)
                    .await;
            }
            info!(
                task_id = %task.id,
                evaluation_id = %eval.id,
                "Git host webhook trigger fired"
            );
            db.record_evaluation_created(&eval).await;
            outcome
                .queued
                .push(QueuedEvaluation::new(task, project_name, eval.id));
        }
        Ok(ApplyOutcome::SkippedSameCommit) => {
            push_skipped(outcome, task, project_name, "same_commit")
        }
        Ok(ApplyOutcome::SkippedConcurrency) => {
            push_skipped(outcome, task, project_name, "concurrency")
        }
        Err(e) => {
            warn!(error = %e, task_id = %task.id, "apply_trigger failed in webhook fan-out");
            push_skipped(outcome, task, project_name, "error");
        }
    }
}

fn push_skipped(
    outcome: &mut WebhookTriggerOutcome,
    task: &MTask,
    project_name: String,
    reason: &str,
) {
    outcome
        .skipped
        .push(SkippedTask::new(task, project_name, reason));
}

/// The gate is failing closed. A possibly-fork PR is gated unless it is same-repo or the actor is a
/// trusted repo writer.
async fn decide_pr_gate(
    ci: &CiContext,
    task: &MTask,
    ctx: Option<&PullRequestApprovalContext>,
) -> Option<ApprovalInfo> {
    let ctx = ctx?;
    let sender_trusted = match ctx.sender.as_deref() {
        Some(sender) if !matches!(ctx.is_fork, Some(false)) => {
            match parse_owner_repo(&task.repository) {
                Some((owner, repo)) => sender_is_trusted(ci, task.id, &owner, &repo, sender).await,
                None => false,
            }
        }
        _ => false,
    };
    gate_decision(ctx, sender_trusted)
}

fn gate_decision(ctx: &PullRequestApprovalContext, sender_trusted: bool) -> Option<ApprovalInfo> {
    if matches!(ctx.is_fork, Some(false)) || sender_trusted {
        return None;
    }
    Some(ApprovalInfo {
        pr_number: ctx.pr_number.unwrap_or(0),
        pr_author: ctx.pr_author.clone().unwrap_or_default(),
    })
}

async fn touch_trigger_last_fired(db: &WebDb, trig: &ept::Model) {
    let now = gradient_types::now();
    if let Err(e) = gradient_db::triggers::set_last_fired(db, trig.clone(), now).await {
        warn!(error = %e, trigger_id = %trig.id, "failed to stamp trigger last_fired_at");
    }
}

fn glob_matches(globs: &[String], name: &str) -> bool {
    if globs.is_empty() {
        return true;
    }
    globs.iter().any(|g| glob_match_pattern(g, name))
}

fn glob_match_pattern(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_recursive(&p, &t, 0, 0)
}

fn glob_match_recursive(p: &[char], t: &[char], pi: usize, ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    if p[pi] == '*' {
        let next_pi = {
            let mut i = pi + 1;
            while i < p.len() && p[i] == '*' {
                i += 1;
            }
            i
        };
        if next_pi == p.len() {
            return true;
        }
        for advance in 0..=(t.len() - ti) {
            if glob_match_recursive(p, t, next_pi, ti + advance) {
                return true;
            }
        }
        false
    } else {
        if ti == t.len() {
            return false;
        }
        if p[pi] == t[ti] {
            glob_match_recursive(p, t, pi + 1, ti + 1)
        } else {
            false
        }
    }
}

async fn project_name_for(db: &WebDb, project_id: ProjectId) -> Option<String> {
    by_id::<EProject, _>(db, project_id)
        .await
        .ok()
        .flatten()
        .map(|o| o.name)
}

#[cfg(test)]
mod tests {
    use super::super::approval::PullRequestApprovalContext;
    use super::{gate_decision, glob_match_pattern, glob_matches};

    fn fork_ctx() -> PullRequestApprovalContext {
        PullRequestApprovalContext {
            pr_number: Some(7),
            pr_author: Some("external".into()),
            is_fork: Some(true),
            sender: Some("maintainer".into()),
        }
    }

    #[test]
    fn gate_same_repo_pr_bypasses() {
        let ctx = PullRequestApprovalContext {
            is_fork: Some(false),
            ..fork_ctx()
        };
        assert!(gate_decision(&ctx, false).is_none());
    }

    #[test]
    fn gate_fork_untrusted_sender_parks() {
        let gate = gate_decision(&fork_ctx(), false).expect("fork PR must park");
        assert_eq!(gate.pr_number, 7);
        assert_eq!(gate.pr_author, "external");
    }

    #[test]
    fn gate_fork_trusted_sender_bypasses() {
        assert!(
            gate_decision(&fork_ctx(), true).is_none(),
            "a trusted maintainer (force-push / command) must bypass the gate"
        );
    }

    #[test]
    fn gate_unknown_fork_status_fails_closed() {
        let ctx = PullRequestApprovalContext {
            is_fork: None,
            sender: None,
            ..fork_ctx()
        };
        assert!(
            gate_decision(&ctx, false).is_some(),
            "uncertain fork status with untrusted sender must park"
        );
    }

    #[test]
    fn glob_patterns_match_lists_and_wildcards() {
        assert!(glob_matches(&[], "main"));
        assert!(glob_matches(&[], "anything"));

        let globs = vec!["main".to_string()];
        assert!(glob_matches(&globs, "main"));
        assert!(!glob_matches(&globs, "develop"));

        let globs = vec!["main".to_string(), "release/*".to_string()];
        assert!(glob_matches(&globs, "main"));
        assert!(glob_matches(&globs, "release/1.0"));
        assert!(!glob_matches(&globs, "develop"));

        assert!(glob_match_pattern("feature/*", "feature/my-branch"));
        assert!(!glob_match_pattern("feature/*", "bugfix/my-branch"));
        assert!(glob_match_pattern("*", "main"));
        assert!(glob_match_pattern("*", ""));
    }
}
