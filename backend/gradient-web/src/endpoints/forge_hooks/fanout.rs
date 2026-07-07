/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Generic webhook fan-out: match active triggers, gate on approval, apply.

use super::approval::{PullRequestApprovalContext, sender_is_trusted};
use super::installation::event_repo_matches_project;
use super::response::{QueuedEvaluation, SkippedProject, WebhookTriggerOutcome};
use gradient_ci::{ApplyInput, ApplyOutcome, ApprovalInfo, apply_trigger, parse_owner_repo};
use gradient_core::ServerState;
use gradient_entity::project_trigger as ept;
use gradient_scheduler::Scheduler;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, DbBackend, EntityTrait, Statement, Value};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy)]
pub(super) enum PushRefKind<'a> {
    Branch(&'a str),
    Tag(&'a str),
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_push_for_integration(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    ref_kind: PushRefKind<'_>,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        event_repo_urls,
        TriggerType::ReporterPush,
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

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_pr_for_integration(
    state: &Arc<ServerState>,
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
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        event_repo_urls,
        TriggerType::ReporterPullRequest,
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
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    tag: Option<&str>,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        event_repo_urls,
        TriggerType::ReporterPush,
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
    /// Proceed to fire `apply_trigger` (push / release / time / polling).
    Fire,
    /// PR trigger matched; the approval gate may still engage on a fork PR.
    FirePr { require_approval: bool },
    /// Config filter did not match - add to skipped with reason "filter".
    SkipFilter,
    /// This trigger type / config shape doesn't apply at all - silently ignore.
    Skip,
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
async fn fan_out_triggers<F>(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    event_repo_urls: &[String],
    trigger_type: TriggerType,
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
    let triggers =
        match load_active_triggers_for_integration(state, integration_id, trigger_type).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "load triggers for integration");
                return WebhookTriggerOutcome::default();
            }
        };

    // Persist PR number/author on the evaluation for every PR trigger (#391);
    // a comment-triggered run already carries a richer source_comment.
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
        let Some(project) = load_trigger_project(state, &trig).await else {
            continue;
        };

        if !event_repo_matches_project(event_repo_urls, &project.repository) {
            continue;
        }

        let filter_result = filter(&cfg);
        if matches!(&filter_result, FilterResult::Skip) {
            continue;
        }

        let org_name = org_name_for(state, project.organization)
            .await
            .unwrap_or_default();

        let pr_require_approval = match filter_result {
            FilterResult::SkipFilter => {
                push_skipped(&mut outcome, &project, org_name, "filter");
                continue;
            }
            FilterResult::Fire => false,
            FilterResult::FirePr { require_approval } => require_approval,
            FilterResult::Skip => continue,
        };
        outcome.projects_scanned += 1;

        let gate_approval = if pr_require_approval {
            decide_pr_gate(state, &project, approval_ctx.as_ref()).await
        } else {
            None
        };

        let input = ApplyInput {
            trigger_id: trig.id,
            trigger_type,
            commit_hash: commit_hash.clone(),
            commit_message: commit_message.clone(),
            author_name: author_name.clone(),
            manual,
            gate_approval,
            repository_override: repository_override.clone(),
            wildcard_override: wildcard_override.clone(),
            source_comment: source_comment.clone(),
            instance_max_storage_gb: state.config.storage.max_storage_gb,
        };

        apply_and_record(
            state,
            scheduler,
            &project,
            &trig,
            input,
            org_name,
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

async fn load_trigger_project(state: &Arc<ServerState>, trig: &ept::Model) -> Option<MProject> {
    match EProject::find_by_id(trig.project).one(&state.web_db).await {
        Ok(Some(p)) => Some(p),
        Ok(None) => {
            warn!(trigger_id = %trig.id, project_id = %trig.project, "project not found for trigger");
            None
        }
        Err(e) => {
            warn!(error = %e, trigger_id = %trig.id, "DB error fetching project for trigger");
            None
        }
    }
}

/// Apply one matched trigger and fold the outcome into `outcome`. Stamps
/// `last_fired_at` for any result so webhook-only triggers don't read "never".
async fn apply_and_record(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    project: &MProject,
    trig: &ept::Model,
    input: ApplyInput,
    org_name: String,
    outcome: &mut WebhookTriggerOutcome,
) {
    let apply_result = apply_trigger(&state.web_db, project, input).await;
    touch_trigger_last_fired(state, trig).await;
    match apply_result {
        Ok(ApplyOutcome::Created {
            evaluation: eval,
            aborted_evaluation,
            aborted_anchors,
        }) => {
            if let Some(aborted_id) = aborted_evaluation {
                scheduler
                    .cancel_evaluation_jobs(aborted_id, &aborted_anchors)
                    .await;
            }
            info!(
                project_id = %project.id,
                evaluation_id = %eval.id,
                "forge webhook trigger fired"
            );
            gradient_ci::actions::dispatch_evaluation_created(&state.ci(), &eval).await;
            outcome.queued.push(QueuedEvaluation {
                project_id: project.id,
                project_name: project.name.clone(),
                organization: org_name,
                evaluation_id: eval.id,
            });
        }
        Ok(ApplyOutcome::SkippedSameCommit) => {
            push_skipped(outcome, project, org_name, "same_commit")
        }
        Ok(ApplyOutcome::SkippedConcurrency) => {
            push_skipped(outcome, project, org_name, "concurrency")
        }
        Err(e) => {
            warn!(error = %e, project_id = %project.id, "apply_trigger failed in webhook fan-out");
            push_skipped(outcome, project, org_name, "error");
        }
    }
}

fn push_skipped(
    outcome: &mut WebhookTriggerOutcome,
    project: &MProject,
    org_name: String,
    reason: &str,
) {
    outcome.skipped.push(SkippedProject {
        project_id: project.id,
        project_name: project.name.clone(),
        organization: org_name,
        reason: reason.into(),
    });
}

/// Resolve whether a PR fire should gate on maintainer approval. Fail-closed:
/// gates a (possibly) fork PR unless it is same-repo or the actor is a trusted
/// repo writer (a maintainer force-push / command runs without re-parking).
async fn decide_pr_gate(
    state: &Arc<ServerState>,
    project: &MProject,
    ctx: Option<&PullRequestApprovalContext>,
) -> Option<ApprovalInfo> {
    let ctx = ctx?;
    let sender_trusted = match ctx.sender.as_deref() {
        Some(sender) if !matches!(ctx.is_fork, Some(false)) => {
            match parse_owner_repo(&project.repository) {
                Some((owner, repo)) => {
                    sender_is_trusted(state, project.id, &owner, &repo, sender).await
                }
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

async fn touch_trigger_last_fired(state: &Arc<ServerState>, trig: &ept::Model) {
    let now = gradient_types::now();
    let mut active: ept::ActiveModel = trig.clone().into();
    active.last_fired_at = Set(Some(now));
    active.updated_at = Set(now);
    if let Err(e) = active.update(&state.web_db).await {
        warn!(error = %e, trigger_id = %trig.id, "failed to stamp trigger last_fired_at");
    }
}

async fn load_active_triggers_for_integration(
    state: &Arc<ServerState>,
    integration_id: IntegrationId,
    trigger_type: TriggerType,
) -> Result<Vec<ept::Model>, sea_orm::DbErr> {
    // Match by org (each org has one inbound integration per forge_type), not by
    // config integration_id: the GitHub App seed migration rewrites integration
    // rows, so a pre-migration trigger's stale UUID would stop matching.
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT pt.* FROM project_trigger pt \
             JOIN project p ON pt.project = p.id \
             JOIN integration i ON i.organization = p.organization \
             WHERE pt.active = true \
               AND pt.trigger_type = {} \
               AND i.id = $1",
            i16::from(trigger_type),
        ),
        [Value::Uuid(Some(Box::new(integration_id.into_inner())))],
    );
    EProjectTrigger::find()
        .from_raw_sql(stmt)
        .all(&state.web_db)
        .await
}

/// Simple glob match: `*` matches any sequence of characters (including none).
/// An empty `globs` list means "match everything".
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

async fn org_name_for(state: &Arc<ServerState>, org_id: OrganizationId) -> Option<String> {
    EOrganization::find_by_id(org_id)
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
        .map(|o| o.name)
}

#[cfg(test)]
mod tests {
    use super::super::WebhookTriggerOutcome;
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
    fn trigger_outcome_default_is_empty() {
        let o = WebhookTriggerOutcome::default();
        assert_eq!(o.projects_scanned, 0);
        assert!(o.queued.is_empty());
        assert!(o.skipped.is_empty());
    }

    #[test]
    fn glob_empty_list_matches_all() {
        assert!(glob_matches(&[], "main"));
        assert!(glob_matches(&[], "anything"));
    }

    #[test]
    fn glob_exact_match() {
        let globs = vec!["main".to_string()];
        assert!(glob_matches(&globs, "main"));
        assert!(!glob_matches(&globs, "develop"));
    }

    #[test]
    fn glob_star_prefix() {
        assert!(glob_match_pattern("feature/*", "feature/my-branch"));
        assert!(!glob_match_pattern("feature/*", "bugfix/my-branch"));
    }

    #[test]
    fn glob_star_only() {
        assert!(glob_match_pattern("*", "main"));
        assert!(glob_match_pattern("*", ""));
    }

    #[test]
    fn glob_version_pattern() {
        assert!(glob_match_pattern("v*", "v1.2.3"));
        assert!(!glob_match_pattern("v*", "1.2.3"));
    }

    #[test]
    fn glob_multiple_patterns() {
        let globs = vec!["main".to_string(), "release/*".to_string()];
        assert!(glob_matches(&globs, "main"));
        assert!(glob_matches(&globs, "release/1.0"));
        assert!(!glob_matches(&globs, "develop"));
    }
}
