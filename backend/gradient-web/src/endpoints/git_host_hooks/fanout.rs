/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::approval::{
    PullRequestApprovalContext, login_can_push_to_repository,
    store_pending_approval_for_untrusted_fork_pull_request_commit_and_post_approval_check,
};
use super::flake_input_push::trigger_input_updates_for_push;
use super::installation::event_repo_matches_task;
use super::response::{
    QueuedEvaluation, SkippedTask, TaskEvaluationNotStartedReason, WebhookTriggerOutcome,
};
use gradient_ci::CiContext;
use gradient_ci::{
    ApplyError, ApplyInput, ApplyOutcome, EvaluationCodeTrust, PathFilterBaseCommit,
    PathFilterCommitComparison, apply_trigger, parse_owner_repo,
    should_fire_trigger_for_path_filter,
};
use gradient_db::lookup::by_id;
use gradient_db::{DbContext, WebDb};
use gradient_entity::evaluation::WalkMode;
use gradient_entity::task_trigger as ept;
use gradient_git_host::{ParsedMergeGroupEvent, ParsedPushEvent, ReleaseTarget};
use gradient_scheduler::Scheduler;
use gradient_types::branch_key;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{info, warn};

pub(super) const CHANGED_PATHS_CHECK_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy)]
enum PushRefKind<'a> {
    Branch(&'a str),
    Tag(&'a str),
}

impl<'a> PushRefKind<'a> {
    fn of(push: &'a ParsedPushEvent) -> Self {
        if push.is_tag {
            Self::Tag(&push.ref_name)
        } else {
            Self::Branch(&push.ref_name)
        }
    }

    fn branch_key(self) -> String {
        match self {
            Self::Branch(name) => branch_key::branch(name),
            Self::Tag(name) => branch_key::tag(name),
        }
    }
}

pub(super) async fn trigger_push_for_connection(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    connection_id: GitHostConnectionId,
    push: &ParsedPushEvent,
) -> WebhookTriggerOutcome {
    let mut outcome = trigger_task_repository_push(ci, scheduler, connection_id, push).await;
    outcome.merge(trigger_input_updates_for_push(ci, connection_id, push).await);
    outcome
}

async fn trigger_task_repository_push(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    connection_id: GitHostConnectionId,
    push: &ParsedPushEvent,
) -> WebhookTriggerOutcome {
    let ref_kind = PushRefKind::of(push);
    let path_filter_base_commit_description = match ref_kind {
        PushRefKind::Branch(_) => Some(PathFilterBaseCommit::NewestEvaluationOnBranch(
            ref_kind.branch_key(),
        )),
        PushRefKind::Tag(_) => None,
    };
    fan_out_triggers(
        ci,
        scheduler,
        connection_id,
        &push.repository_urls,
        TriggerType::ReporterPush,
        ref_kind.branch_key(),
        EventCommit::Known(push.commit_hash.clone()),
        push.commit_message.clone(),
        push.author_name.clone(),
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
        true,
        None,
        None,
        path_filter_base_commit_description,
    )
    .await
}

/// The queue branch is the branch key, and the push event for that branch dedups against this
/// evaluation. The base branch picks the trigger.
pub(super) async fn trigger_merge_group_for_connection(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    connection_id: GitHostConnectionId,
    group: &ParsedMergeGroupEvent,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        ci,
        scheduler,
        connection_id,
        &group.repository_urls,
        TriggerType::ReporterPush,
        branch_key::branch(&group.queue_branch),
        EventCommit::Known(group.commit_hash.clone()),
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
        false,
        None,
        None,
        None,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_pr_for_connection(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    connection_id: GitHostConnectionId,
    event_repo_urls: &[String],
    branch: Option<&str>,
    base_branch: Option<&str>,
    action: &str,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    approval_ctx: PullRequestApprovalContext,
    head_repo_clone_url: Option<RepositoryUrl>,
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
    let path_filter_base_commit_description = base_branch
        .filter(|_| !manual)
        .map(|base| PathFilterBaseCommit::TargetBranchTip(base.to_owned()));
    fan_out_triggers(
        ci,
        scheduler,
        connection_id,
        event_repo_urls,
        TriggerType::ReporterPullRequest,
        pull_request_key,
        EventCommit::Known(commit_hash),
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPullRequest {
                branches,
                actions,
                require_approval,
                ..
            } => {
                if !pr_action_matches(actions, &action_owned) {
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
        false,
        wildcard_override,
        source_comment,
        path_filter_base_commit_description,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub(super) async fn trigger_release_for_connection(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    connection_id: GitHostConnectionId,
    event_repo_urls: &[String],
    tag: Option<&str>,
    target: &ReleaseTarget,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    let commit = match target {
        ReleaseTarget::Commit(hash) => EventCommit::Known(hash.clone()),
        ReleaseTarget::Tag(name) => EventCommit::TipOfTag(name.clone()),
    };
    fan_out_triggers(
        ci,
        scheduler,
        connection_id,
        event_repo_urls,
        TriggerType::ReporterPush,
        tag.map_or_else(|| branch_key::NO_BRANCH.to_owned(), branch_key::tag),
        commit,
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
        false,
        None,
        None,
        None,
    )
    .await
}

enum EventCommit {
    Known(Vec<u8>),
    TipOfTag(String),
}

impl EventCommit {
    async fn for_task(&self, db: &DbContext, task: &MTask) -> Option<Vec<u8>> {
        let tag = match self {
            Self::Known(hash) => return Some(hash.clone()),
            Self::TipOfTag(tag) => tag,
        };
        gradient_sources::remote_tag_commit(db, task, tag)
            .await
            .inspect_err(|e| warn!(error = %e, task_id = %task.id, tag, "Resolve release tags for the task repository"))
            .ok()
    }
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
    connection_id: GitHostConnectionId,
    event_repo_urls: &[String],
    trigger_type: TriggerType,
    branch: String,
    commit: EventCommit,
    commit_message: Option<String>,
    author_name: Option<String>,
    filter: F,
    approval_ctx: Option<PullRequestApprovalContext>,
    repository_override: Option<RepositoryUrl>,
    manual: bool,
    honors_skip_marker: bool,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
    path_filter_base_commit_description: Option<PathFilterBaseCommit>,
) -> WebhookTriggerOutcome
where
    F: Fn(&TriggerConfig) -> FilterResult,
{
    let path_filter_check_deadline = Instant::now() + CHANGED_PATHS_CHECK_TIMEOUT;
    let pr_base_branch_name = match &path_filter_base_commit_description {
        Some(PathFilterBaseCommit::TargetBranchTip(branch)) => Some(branch.clone()),
        _ => None,
    };
    let triggers = match gradient_db::triggers::list_active_triggers_of_type_on_tasks_using_git_host_connection(
        &ci.db.web_db,
        connection_id,
        trigger_type,
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Load triggers for connections to Git hosts.");
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
                push_skipped(
                    &mut outcome,
                    &task,
                    project_name,
                    TaskEvaluationNotStartedReason::Filter,
                );
                continue;
            }
            FilterResult::Fire => None,
            FilterResult::FirePr { require_approval } => Some(require_approval),
            FilterResult::Skip => continue,
        };
        outcome.tasks_scanned += 1;

        let Some(commit_hash) = commit.for_task(&ci.db, &task).await else {
            push_skipped(
                &mut outcome,
                &task,
                project_name,
                TaskEvaluationNotStartedReason::TagNotFound,
            );
            continue;
        };

        let head_repository = repository_override
            .as_ref()
            .map(|url| url.git_connection_url().to_owned());
        let start_decision = match pr_require_approval {
            None => PullRequestCommitEvaluationStartDecision::StartWithEvaluationCodeTrust(
                EvaluationCodeTrust::CodeWithProjectCredentials,
            ),
            Some(require_approval) => {
                decide_evaluation_code_trust_from_approval_context_repository_and_push_rights(
                    approval_ctx.as_ref(),
                    &task.repository,
                    &commit_hash,
                    require_approval,
                    |owner: String, repo: String, login: String| async move {
                        login_can_push_to_repository(ci, task.id, &owner, &repo, &login).await
                    },
                )
                .await
            }
        };
        let code_trust = match start_decision {
            PullRequestCommitEvaluationStartDecision::StartWithEvaluationCodeTrust(code_trust) => {
                code_trust
            }
            PullRequestCommitEvaluationStartDecision::ForkPullRequestAwaitingMaintainerApproval {
                pr_number: None,
                ..
            } => {
                warn!(
                    task_id = %task.id,
                    "Pull request event with no pull request number -> skip commits for fork pull requests that wait for maintainer approval."
                );
                push_skipped(
                    &mut outcome,
                    &task,
                    project_name,
                    TaskEvaluationNotStartedReason::Error,
                );
                continue;
            }
            PullRequestCommitEvaluationStartDecision::ForkPullRequestAwaitingMaintainerApproval {
                pr_number: Some(pr_number),
                pr_author,
            } => {
                if let Some(reason) =
                    require_maintainer_approval_for_untrusted_fork_pull_request_commit(
                        ci,
                        &task,
                        &trig,
                        pr_number,
                        pr_author,
                        &commit_hash,
                        head_repository.clone(),
                        pr_base_branch_name.clone(),
                        commit_message.clone(),
                        author_name.clone(),
                    )
                    .await
                {
                    push_skipped(&mut outcome, &task, project_name, reason);
                    continue;
                }
                EvaluationCodeTrust::ForkPullRequestCodeApprovedForHeadCommit
            }
        };

        if let (Some(base), Some(filter)) = (
            path_filter_base_commit_description.as_ref(),
            cfg.path_filter(),
        ) && !should_fire_trigger_for_path_filter(
            &ci.db.web_db,
            &ci.db,
            &task,
            filter,
            PathFilterCommitComparison {
                base,
                head: &commit_hash,
                head_repository: head_repository.as_deref(),
            },
            path_filter_check_deadline.saturating_duration_since(Instant::now()),
        )
        .await
        {
            push_skipped(
                &mut outcome,
                &task,
                project_name,
                TaskEvaluationNotStartedReason::NoChangedFileMatchesPathFilter,
            );
            continue;
        }

        let input = ApplyInput {
            trigger_id: Some(trig.id),
            trigger_type,
            branch: branch.clone(),
            commit_hash,
            commit_message: commit_message.clone(),
            author_name: author_name.clone(),
            manual,
            honors_skip_marker,
            code_trust,
            repository_override: repository_override.clone(),
            wildcard_override: wildcard_override.clone(),
            source_comment: source_comment.clone(),
            started_by: None,
            walk_mode: WalkMode::Pruned,
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

pub(super) fn parse_trigger_config(trig: &ept::Model) -> Option<TriggerConfig> {
    match TriggerConfig::parse_row(trig.trigger_type, &trig.config) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!(trigger_id = %trig.id, error = %e, "skip trigger with invalid config");
            None
        }
    }
}

pub(super) async fn load_trigger_task(db: &WebDb, trig: &ept::Model) -> Option<MTask> {
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
    record_trigger_apply_outcome(db, scheduler, task, apply_result, project_name, outcome).await;
}

pub(super) async fn record_trigger_apply_outcome(
    db: &DbContext,
    scheduler: &Arc<Scheduler>,
    task: &MTask,
    apply_result: Result<ApplyOutcome, ApplyError>,
    project_name: String,
    outcome: &mut WebhookTriggerOutcome,
) {
    match apply_result {
        Ok(ApplyOutcome::Created {
            evaluation: eval,
            aborted_evaluations,
        }) => {
            for (aborted_id, hard_abort) in aborted_evaluations {
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
        Ok(ApplyOutcome::SkippedSameCommit) => push_skipped(
            outcome,
            task,
            project_name,
            TaskEvaluationNotStartedReason::SameCommit,
        ),
        Ok(ApplyOutcome::SkippedConcurrency) => push_skipped(
            outcome,
            task,
            project_name,
            TaskEvaluationNotStartedReason::Concurrency,
        ),
        Ok(ApplyOutcome::SkippedByCommitMessage) => push_skipped(
            outcome,
            task,
            project_name,
            TaskEvaluationNotStartedReason::CommitMessage,
        ),
        Err(e) => {
            warn!(error = %e, task_id = %task.id, "apply_trigger failed in webhook fan-out");
            push_skipped(
                outcome,
                task,
                project_name,
                TaskEvaluationNotStartedReason::Error,
            );
        }
    }
}

pub(super) fn push_skipped(
    outcome: &mut WebhookTriggerOutcome,
    task: &MTask,
    project_name: String,
    reason: TaskEvaluationNotStartedReason,
) {
    outcome
        .skipped
        .push(SkippedTask::new(task, project_name, reason));
}

async fn decide_evaluation_code_trust_from_approval_context_repository_and_push_rights<
    CanPushToRepoLookupFuture,
>(
    ctx: Option<&PullRequestApprovalContext>,
    repository: &str,
    commit_hash: &[u8],
    require_approval: bool,
    login_can_push_to_repo_lookup: impl FnOnce(String, String, String) -> CanPushToRepoLookupFuture,
) -> PullRequestCommitEvaluationStartDecision
where
    CanPushToRepoLookupFuture: Future<Output = bool>,
{
    let unknown = PullRequestApprovalContext::default();
    let ctx = ctx.unwrap_or(&unknown);
    let author_owning_head_repo = match (
        ctx.is_fork,
        ctx.pr_author.as_deref(),
        ctx.head_repo_owner_login.as_deref(),
    ) {
        (Some(true), Some(author), Some(owner)) if author.eq_ignore_ascii_case(owner) => {
            Some(author)
        }
        _ => None,
    };
    let author_can_push = match (author_owning_head_repo, parse_owner_repo(repository)) {
        (Some(author), Some((owner, repo))) => {
            login_can_push_to_repo_lookup(owner, repo, author.to_owned()).await
        }
        _ => false,
    };
    decide_evaluation_code_trust_for_pull_request_commit(
        ctx,
        commit_hash,
        require_approval,
        author_can_push,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PullRequestCommitEvaluationStartDecision {
    StartWithEvaluationCodeTrust(EvaluationCodeTrust),
    ForkPullRequestAwaitingMaintainerApproval {
        pr_number: Option<u64>,
        pr_author: String,
    },
}

fn decide_evaluation_code_trust_for_pull_request_commit(
    ctx: &PullRequestApprovalContext,
    commit_hash: &[u8],
    require_approval: bool,
    author_can_push: bool,
) -> PullRequestCommitEvaluationStartDecision {
    let code_trust = if matches!(ctx.is_fork, Some(false)) || author_can_push {
        EvaluationCodeTrust::CodeWithProjectCredentials
    } else if ctx.approved_head_commit_sha.as_deref() == Some(commit_hash) {
        EvaluationCodeTrust::ForkPullRequestCodeApprovedForHeadCommit
    } else if !require_approval && !ctx.unknown_author_requires_approval {
        EvaluationCodeTrust::UntrustedForkPullRequestCodeWithoutApproval
    } else {
        return PullRequestCommitEvaluationStartDecision::ForkPullRequestAwaitingMaintainerApproval {
            pr_number: ctx.pr_number,
            pr_author: ctx.pr_author.clone().unwrap_or_default(),
        };
    };
    PullRequestCommitEvaluationStartDecision::StartWithEvaluationCodeTrust(code_trust)
}

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
async fn require_maintainer_approval_for_untrusted_fork_pull_request_commit(
    ci: &CiContext,
    task: &MTask,
    trig: &ept::Model,
    pr_number: u64,
    pr_author: String,
    commit_hash: &[u8],
    head_repo_clone_url: Option<String>,
    pr_base_branch_name: Option<String>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> Option<TaskEvaluationNotStartedReason> {
    let approved = gradient_db::fork_pull_request_approvals::is_commit_approved_by_maintainer_for_task(
        &ci.db.web_db,
        task.id,
        pr_number,
        commit_hash,
    )
    .await
    .unwrap_or_else(|e| {
        warn!(error = %e, task_id = %task.id, "Check whether a maintainer approved the commit of a fork pull request");
        false
    });
    if approved {
        return None;
    }

    let pending = MForkPullRequestApproval {
        id: PullRequestApprovalId::now_v7(),
        task: task.id,
        trigger: trig.id,
        pr_number: pr_number as i64,
        pr_author,
        commit_hash: commit_hash.to_vec(),
        head_repo_clone_url,
        pr_base_branch_name,
        commit_message,
        author_name,
        check_run_id: None,
        approving_maintainer_login: None,
        approved_at: None,
        created_at: gradient_types::now(),
    };
    let stored =
        store_pending_approval_for_untrusted_fork_pull_request_commit_and_post_approval_check(
            &ci.db,
            task.project,
            pending,
        )
        .await;
    touch_trigger_last_fired(&ci.db.web_db, trig).await;
    Some(match stored {
        Ok(()) => TaskEvaluationNotStartedReason::ForkPullRequestCommitWaitsForMaintainerApproval,
        Err(e) => {
            warn!(error = %e, task_id = %task.id, "Save pending fork pull request approval");
            TaskEvaluationNotStartedReason::Error
        }
    })
}

pub(super) async fn touch_trigger_last_fired(db: &WebDb, trig: &ept::Model) {
    let now = gradient_types::now();
    if let Err(e) = gradient_db::triggers::set_last_fired(db, trig.clone(), now).await {
        warn!(error = %e, trigger_id = %trig.id, "failed to stamp trigger last_fired_at");
    }
}

fn pr_action_matches(actions: &[String], action: &str) -> bool {
    actions.is_empty() || actions.iter().any(|a| a == action)
}

fn glob_matches(globs: &[String], name: &str) -> bool {
    if globs.is_empty() {
        return true;
    }
    globs
        .iter()
        .any(|g| gradient_types::wildcard::segment_covers(g, name))
}

pub(super) async fn project_name_for(db: &WebDb, project_id: ProjectId) -> Option<String> {
    by_id::<EProject, _>(db, project_id)
        .await
        .ok()
        .flatten()
        .map(|o| o.name)
}

#[cfg(test)]
mod tests {
    use super::super::approval::PullRequestApprovalContext;
    use super::super::commands::{
        CommitSpecifiedInCommentCommand, build_approval_context_for_gradient_run_comment,
    };
    use super::{
        PullRequestCommitEvaluationStartDecision,
        decide_evaluation_code_trust_for_pull_request_commit,
        decide_evaluation_code_trust_from_approval_context_repository_and_push_rights,
        glob_matches, pr_action_matches,
    };
    use gradient_ci::EvaluationCodeTrust;
    use gradient_git_host::ParsedPullRequestEvent;
    use std::sync::Mutex;

    #[test]
    fn an_empty_action_list_fires_on_any_pull_request_action() {
        assert!(pr_action_matches(&[], "edited"));
        let listed = vec!["opened".to_owned()];
        assert!(pr_action_matches(&listed, "opened"));
        assert!(!pr_action_matches(&listed, "closed"));
    }

    const HEAD: [u8; 20] = [0xab; 20];

    fn fork_ctx() -> PullRequestApprovalContext {
        PullRequestApprovalContext {
            pr_number: Some(7),
            pr_author: Some("external".into()),
            is_fork: Some(true),
            head_repo_owner_login: Some("external".into()),
            ..PullRequestApprovalContext::default()
        }
    }

    fn fork_pull_request_awaiting_maintainer_approval_for_author(
        pr_author: &str,
    ) -> PullRequestCommitEvaluationStartDecision {
        PullRequestCommitEvaluationStartDecision::ForkPullRequestAwaitingMaintainerApproval {
            pr_number: Some(7),
            pr_author: pr_author.into(),
        }
    }

    fn awaiting_approval() -> PullRequestCommitEvaluationStartDecision {
        fork_pull_request_awaiting_maintainer_approval_for_author("external")
    }

    fn start_with_evaluation_code_trust_decision(
        code_trust: EvaluationCodeTrust,
    ) -> PullRequestCommitEvaluationStartDecision {
        PullRequestCommitEvaluationStartDecision::StartWithEvaluationCodeTrust(code_trust)
    }

    #[test]
    fn same_repository_pull_request_runs_with_project_credentials() {
        let ctx = PullRequestApprovalContext {
            is_fork: Some(false),
            ..fork_ctx()
        };
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(&ctx, &HEAD, true, false),
            start_with_evaluation_code_trust_decision(
                EvaluationCodeTrust::CodeWithProjectCredentials
            )
        );
    }

    #[test]
    fn fork_pull_request_commit_without_pr_number_is_awaiting_maintainer_approval() {
        let ctx = PullRequestApprovalContext {
            pr_number: None,
            ..fork_ctx()
        };
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(&ctx, &HEAD, true, false),
            PullRequestCommitEvaluationStartDecision::ForkPullRequestAwaitingMaintainerApproval {
                pr_number: None,
                pr_author: "external".into(),
            }
        );
    }

    #[test]
    fn fork_pull_request_author_without_push_rights_waits_for_maintainer_approval_even_when_maintainer_sends_update_branch_event()
     {
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(&fork_ctx(), &HEAD, true, false),
            awaiting_approval()
        );
    }

    #[test]
    fn fork_pull_request_author_with_push_rights_runs_with_project_credentials() {
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(&fork_ctx(), &HEAD, true, true),
            start_with_evaluation_code_trust_decision(
                EvaluationCodeTrust::CodeWithProjectCredentials
            )
        );
    }

    #[test]
    fn untrusted_fork_pull_request_runs_without_project_credentials_when_approval_is_not_required()
    {
        let PullRequestCommitEvaluationStartDecision::StartWithEvaluationCodeTrust(trust) =
            decide_evaluation_code_trust_for_pull_request_commit(&fork_ctx(), &HEAD, false, false)
        else {
            panic!("expected an evaluation start");
        };
        assert_eq!(
            trust,
            EvaluationCodeTrust::UntrustedForkPullRequestCodeWithoutApproval
        );
        assert!(!trust.allows_project_credentials_and_privileged_actions());
    }

    #[test]
    fn maintainer_approval_counts_only_for_given_head_commit() {
        let approved = PullRequestApprovalContext {
            approved_head_commit_sha: Some(HEAD.to_vec()),
            ..fork_ctx()
        };
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(&approved, &HEAD, true, false),
            start_with_evaluation_code_trust_decision(
                EvaluationCodeTrust::ForkPullRequestCodeApprovedForHeadCommit
            )
        );
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(
                &approved,
                &[0xcd; 20],
                true,
                false
            ),
            awaiting_approval()
        );
    }

    #[test]
    fn test_unknown_is_fork_waits_for_maintainer_approval() {
        let ctx = PullRequestApprovalContext {
            is_fork: None,
            ..fork_ctx()
        };
        assert_eq!(
            decide_evaluation_code_trust_for_pull_request_commit(&ctx, &HEAD, true, false),
            awaiting_approval()
        );
    }

    async fn decide_bare_gradient_run_trust_for_member_with_push_rights_and_snapshot_is_fork(
        is_fork: Option<bool>,
        require_approval: bool,
    ) -> (PullRequestCommitEvaluationStartDecision, Vec<String>) {
        let snapshot = gradient_ci::PullRequestSnapshot {
            head_sha: hex::encode(HEAD),
            head_branch: "patch-1".into(),
            head_clone_url: None,
            is_fork,
            title: None,
        };
        let ctx = build_approval_context_for_gradient_run_comment(
            7,
            &snapshot,
            &CommitSpecifiedInCommentCommand::NoCommitHashSpecified,
            &HEAD,
        );
        let looked_up = Mutex::new(Vec::new());
        let trust = decide_evaluation_code_trust_from_approval_context_repository_and_push_rights(
            Some(&ctx),
            "https://github.com/org/repo",
            &HEAD,
            require_approval,
            |owner: String, repo: String, login: String| {
                looked_up
                    .lock()
                    .unwrap()
                    .push(format!("{owner}/{repo}:{login}"));
                std::future::ready(true)
            },
        )
        .await;
        (trust, looked_up.into_inner().unwrap())
    }

    #[tokio::test]
    async fn gradient_run_on_pull_request_with_deleted_fork_waits_for_maintainer_approval() {
        assert_eq!(
            decide_bare_gradient_run_trust_for_member_with_push_rights_and_snapshot_is_fork(
                None, true
            )
            .await,
            (
                fork_pull_request_awaiting_maintainer_approval_for_author(""),
                Vec::new()
            )
        );
        let (decision, looked_up) =
            decide_bare_gradient_run_trust_for_member_with_push_rights_and_snapshot_is_fork(
                None, false,
            )
            .await;
        assert_eq!(
            decision,
            start_with_evaluation_code_trust_decision(
                EvaluationCodeTrust::UntrustedForkPullRequestCodeWithoutApproval
            )
        );
        assert!(looked_up.is_empty());
    }

    #[tokio::test]
    async fn gradient_run_on_same_repository_pull_request_uses_project_credentials() {
        assert_eq!(
            decide_bare_gradient_run_trust_for_member_with_push_rights_and_snapshot_is_fork(
                Some(false),
                true
            )
            .await,
            (
                start_with_evaluation_code_trust_decision(
                    EvaluationCodeTrust::CodeWithProjectCredentials
                ),
                Vec::new()
            )
        );
    }

    fn build_pull_request_webhook_json_body_for_sender_author_and_repositories(
        sender: &str,
        author: &str,
        head_repository: &str,
    ) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": "synchronize",
            "sender": { "login": sender },
            "pull_request": {
                "number": 7,
                "user": { "login": author },
                "head": {
                    "sha": hex::encode(HEAD),
                    "ref": "patch-1",
                    "repo": {
                        "full_name": head_repository,
                        "clone_url": format!("https://github.com/{head_repository}.git"),
                    },
                },
                "base": { "sha": "0".repeat(40), "ref": "main", "repo": { "full_name": "org/repo" } },
            },
            "repository": {
                "clone_url": "https://github.com/org/repo.git",
                "ssh_url": "git@github.com:org/repo.git",
                "full_name": "org/repo",
            },
        }))
        .unwrap()
    }

    async fn parse_webhook_decide_trust_and_record_push_rights_lookups_for_allowed_logins(
        body: Vec<u8>,
        parse: fn(&[u8]) -> Option<ParsedPullRequestEvent>,
        require_approval: bool,
        writers: &[&str],
    ) -> (PullRequestCommitEvaluationStartDecision, Vec<String>) {
        let parsed = parse(&body).unwrap();
        let ctx = PullRequestApprovalContext::from(&parsed);
        let looked_up = Mutex::new(Vec::new());
        let trust = decide_evaluation_code_trust_from_approval_context_repository_and_push_rights(
            Some(&ctx),
            "https://github.com/org/repo",
            &HEAD,
            require_approval,
            |owner: String, repo: String, login: String| {
                looked_up
                    .lock()
                    .unwrap()
                    .push(format!("{owner}/{repo}:{login}"));
                std::future::ready(writers.contains(&login.as_str()))
            },
        )
        .await;
        (trust, looked_up.into_inner().unwrap())
    }

    #[tokio::test]
    async fn maintainer_sent_fork_pull_request_for_external_author_uses_author_login_for_push_rights_lookup_and_waits_for_approval_without_project_credentials()
     {
        let body = build_pull_request_webhook_json_body_for_sender_author_and_repositories(
            "maintainer",
            "external",
            "external/repo",
        );

        let (trust, looked_up) =
            parse_webhook_decide_trust_and_record_push_rights_lookups_for_allowed_logins(
                body,
                ParsedPullRequestEvent::from_github,
                true,
                &["maintainer"],
            )
            .await;

        assert_eq!(looked_up, vec!["org/repo:external".to_owned()]);
        assert_eq!(trust, awaiting_approval());
    }

    #[tokio::test]
    async fn github_pull_request_by_maintainer_with_head_repository_owned_by_another_user_waits_for_approval_and_does_not_look_up_push_rights()
     {
        let body = build_pull_request_webhook_json_body_for_sender_author_and_repositories(
            "attacker",
            "maintainer",
            "attacker/repo",
        );

        let (trust, looked_up) =
            parse_webhook_decide_trust_and_record_push_rights_lookups_for_allowed_logins(
                body,
                ParsedPullRequestEvent::from_github,
                true,
                &["maintainer"],
            )
            .await;

        assert!(looked_up.is_empty(), "{looked_up:?}");
        assert_eq!(
            trust,
            fork_pull_request_awaiting_maintainer_approval_for_author("maintainer")
        );
    }

    #[tokio::test]
    async fn test_fork_pr_author_owns_head_repo_and_can_push_to_base_runs_with_project_credentials()
    {
        let body = build_pull_request_webhook_json_body_for_sender_author_and_repositories(
            "attacker",
            "Maintainer",
            "maintainer/repo",
        );

        let (trust, looked_up) =
            parse_webhook_decide_trust_and_record_push_rights_lookups_for_allowed_logins(
                body,
                ParsedPullRequestEvent::from_github,
                true,
                &["Maintainer"],
            )
            .await;

        assert_eq!(looked_up, vec!["org/repo:Maintainer".to_owned()]);
        assert_eq!(
            trust,
            start_with_evaluation_code_trust_decision(
                EvaluationCodeTrust::CodeWithProjectCredentials
            )
        );
    }

    #[tokio::test]
    async fn test_gitea_fork_pull_request_without_push_rights_waits_for_maintainer_approval_without_project_credentials()
     {
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "synchronized",
            "sender": { "username": "maintainer" },
            "pull_request": {
                "number": 7,
                "user": { "username": "external" },
                "head": {
                    "sha": hex::encode(HEAD),
                    "ref": "patch-1",
                    "label": "patch-1",
                    "repo": {
                        "full_name": "external/repo",
                        "clone_url": "https://gitea.example.com/external/repo.git",
                    },
                },
                "base": { "sha": "0".repeat(40), "ref": "main", "repo": { "full_name": "org/repo" } },
            },
            "repository": {
                "clone_url": "https://gitea.example.com/org/repo.git",
                "ssh_url": "git@gitea.example.com:org/repo.git",
                "full_name": "org/repo",
            },
        }))
        .unwrap();

        let (trust, looked_up) =
            parse_webhook_decide_trust_and_record_push_rights_lookups_for_allowed_logins(
                body,
                ParsedPullRequestEvent::from_gitea,
                true,
                &["maintainer"],
            )
            .await;

        assert_eq!(looked_up, vec!["org/repo:external".to_owned()]);
        assert_eq!(trust, awaiting_approval());
    }

    #[tokio::test]
    async fn gitlab_fork_merge_request_waits_for_maintainer_approval_when_require_approval_is_false()
     {
        let body = serde_json::to_vec(&serde_json::json!({
            "object_attributes": {
                "action": "update",
                "iid": 7,
                "source_branch": "feat",
                "source_project_id": 99,
                "target_project_id": 1,
                "last_commit": { "id": hex::encode(HEAD) },
            },
            "project": { "http_url": "https://gitlab.example.com/org/repo.git" },
            "user": { "username": "maintainer" },
        }))
        .unwrap();

        let (trust, looked_up) =
            parse_webhook_decide_trust_and_record_push_rights_lookups_for_allowed_logins(
                body,
                ParsedPullRequestEvent::from_gitlab,
                false,
                &["maintainer"],
            )
            .await;

        assert!(looked_up.is_empty(), "{looked_up:?}");
        assert_eq!(
            trust,
            fork_pull_request_awaiting_maintainer_approval_for_author("")
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

        let feature = vec!["feature/*".to_string()];
        assert!(glob_matches(&feature, "feature/my-branch"));
        assert!(glob_matches(&feature, "feature/nested/branch"));
        assert!(!glob_matches(&feature, "bugfix/my-branch"));
        assert!(glob_matches(&["*".to_string()], "main"));
        assert!(glob_matches(&["*".to_string()], ""));
        assert!(glob_matches(&["**".to_string()], "a/b"));
        assert!(glob_matches(&["re*se/*.x".to_string()], "release/v1.x"));
        assert!(!glob_matches(&["re*se/*.x".to_string()], "release/v1.y"));
        assert!(glob_matches(&["*-hotfix".to_string()], "-hotfix"));
    }

    #[test]
    fn branch_glob_with_many_stars_finishes_quickly_and_does_not_match() {
        let pattern = vec!["*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b".to_string()];
        assert!(!glob_matches(&pattern, &"a".repeat(60)));
    }
}
