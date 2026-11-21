/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::commands::{
    first_task_with_reporter, github_installation_id_from_comment_body,
    load_active_pull_request_tasks_for_event_repository,
};
use super::fanout::{
    CHANGED_PATHS_CHECK_TIMEOUT, parse_trigger_config, project_name_for,
    record_trigger_apply_outcome, touch_trigger_last_fired,
};
use super::installation::resolve_github_app_targets;
use super::payloads::GithubCheckRunRequestedAction;
use super::response::{TaskEvaluationNotStartedReason, WebhookTriggerOutcome};
use gradient_ci::CiContext;
use gradient_ci::{
    APPROVAL_ACTION_ID, ApplyError, ApplyInput, ApplyOutcome, EvaluationCodeTrust,
    PathFilterBaseCommit, PathFilterCommitComparison, apply_trigger,
    should_fire_trigger_for_path_filter,
};
use gradient_db::fork_pull_request_approvals as approvals;
use gradient_db::lookup::by_id;
use gradient_db::{DbContext, WebDb};
use gradient_entity::evaluation::WalkMode;
use gradient_git_host::webhook::ApprovedCommitSource;
use gradient_git_host::{ParsedPullRequestEvent, ParsedPullRequestReviewEvent};
use gradient_scheduler::Scheduler;
use gradient_types::branch_key;
use gradient_types::events::fork_pull_request_approval_events::{Phase, Reported};
use gradient_types::input::vec_to_hex;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::{IntoActiveModel, Set, TransactionTrait};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

const PULL_REQUEST_REVIEW_WEBHOOK_REVIEWER_PUSH_CHECK_AND_APPROVED_COMMIT_READ_TIME_LIMIT:
    Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Default)]
pub(super) struct PullRequestApprovalContext {
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub is_fork: Option<bool>,
    pub head_repo_owner_login: Option<String>,
    pub unknown_author_requires_approval: bool,
    pub approved_head_commit_sha: Option<Vec<u8>>,
}

impl From<&ParsedPullRequestEvent> for PullRequestApprovalContext {
    fn from(parsed: &ParsedPullRequestEvent) -> Self {
        Self {
            pr_number: parsed.pr_number,
            pr_author: parsed.pr_author.clone(),
            is_fork: parsed.is_fork,
            head_repo_owner_login: parsed.head_repo_owner_login.clone(),
            unknown_author_requires_approval: parsed.unknown_author_requires_approval,
            approved_head_commit_sha: None,
        }
    }
}

fn approval_event(
    approval: &MForkPullRequestApproval,
    project: ProjectId,
    phase: Phase,
) -> Reported {
    Reported {
        approval_id: approval.id,
        phase,
        task: approval.task,
        project,
        pr_number: approval.pr_number as u64,
        pr_author: approval.pr_author.clone(),
        commit: vec_to_hex(&approval.commit_hash),
        approving_maintainer_login: approval.approving_maintainer_login.clone(),
    }
}

pub(super) async fn store_pending_approval_for_untrusted_fork_pull_request_commit_and_post_approval_check(
    db: &DbContext,
    project: ProjectId,
    pending: MForkPullRequestApproval,
) -> Result<(), sea_orm::DbErr> {
    let pr_base_branch_name = pending.pr_base_branch_name.clone();
    let stored =
        approvals::add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
            &db.web_db, pending,
        )
        .await?;
    let stored = if stored.pr_base_branch_name == pr_base_branch_name {
        stored
    } else {
        let mut pending_fork_pull_request_approval_active_model = stored.into_active_model();
        pending_fork_pull_request_approval_active_model.pr_base_branch_name =
            Set(pr_base_branch_name);
        gradient_db::rows::update::<EForkPullRequestApproval, _>(
            &db.web_db,
            pending_fork_pull_request_approval_active_model,
        )
        .await?
    };
    db.record(approval_event(
        &stored,
        project,
        Phase::NeedsMaintainerApproval,
    ))
    .await;
    Ok(())
}

pub(super) async fn approve_approval_and_start_approved_commit_evaluation(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    approval: PullRequestApprovalId,
    maintainer_login: &str,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
) -> bool {
    let Some(pending) = by_id::<EForkPullRequestApproval, _>(&ci.db.web_db, approval)
        .await
        .ok()
        .flatten()
        .filter(|row| row.approved_at.is_none())
    else {
        return false;
    };
    let Some(task) = by_id::<ETask, _>(&ci.db.web_db, pending.task)
        .await
        .ok()
        .flatten()
    else {
        return false;
    };
    let Some(trigger) = by_id::<ETaskTrigger, _>(&ci.db.web_db, pending.trigger)
        .await
        .ok()
        .flatten()
        .filter(|trigger| trigger.active)
    else {
        return false;
    };

    if let Some(base_branch_name) = pending.pr_base_branch_name.clone()
        && let Some(config) = parse_trigger_config(&trigger)
        && let Some(filter) = config.path_filter()
        && !should_fire_trigger_for_path_filter(
            &ci.db.web_db,
            &ci.db,
            &task,
            filter,
            PathFilterCommitComparison {
                base: &PathFilterBaseCommit::TargetBranchTip(base_branch_name),
                head: &pending.commit_hash,
                head_repository: pending.head_repo_clone_url.as_deref(),
            },
            CHANGED_PATHS_CHECK_TIMEOUT,
        )
        .await
    {
        return record_approval_approved_without_evaluation_when_no_path_filter_match(
            ci,
            &task,
            &trigger,
            approval,
            maintainer_login,
        )
        .await;
    }

    let pr_number = pending.pr_number as u64;
    let source_comment = source_comment.unwrap_or_else(
        || serde_json::json!({ "pr_number": pr_number, "pr_author": pending.pr_author }),
    );
    let input = ApplyInput {
        trigger_id: Some(trigger.id),
        trigger_type: TriggerType::ReporterPullRequest,
        branch: branch_key::pull_request(pr_number),
        commit_hash: pending.commit_hash,
        commit_message: pending.commit_message,
        author_name: pending.author_name,
        manual: true,
        honors_skip_marker: false,
        code_trust: EvaluationCodeTrust::ForkPullRequestCodeApprovedForHeadCommit,
        repository_override: pending.head_repo_clone_url.and_then(|url| url.parse().ok()),
        wildcard_override,
        source_comment: Some(source_comment),
        started_by: None,
        walk_mode: WalkMode::Pruned,
    };
    let (approved, apply_result) =
        match approve_pending_fork_pull_request_approval_and_apply_trigger_in_transaction(
            &ci.db.web_db,
            &task,
            approval,
            maintainer_login,
            input,
        )
        .await
        {
            Ok(None) => return false,
            Ok(Some((approved, applied))) => (Some(approved), Ok(applied)),
            Err(e) => (None, Err(e)),
        };
    touch_trigger_last_fired(&ci.db.web_db, &trigger).await;
    let project_name = project_name_for(&ci.db.web_db, task.project)
        .await
        .unwrap_or_default();
    let mut outcome = WebhookTriggerOutcome::default();
    record_trigger_apply_outcome(
        &ci.db,
        scheduler,
        &task,
        apply_result,
        project_name,
        &mut outcome,
    )
    .await;
    let Some(approved) = approved.filter(|_| !outcome.queued.is_empty()) else {
        info!(
            %approval,
            task_id = %task.id,
            pr_number,
            maintainer = maintainer_login,
            skipped = ?outcome.skipped,
            "The approval is still pending because the evaluation didn't start."
        );
        return false;
    };
    ci.db
        .record(approval_event(
            &approved,
            task.project,
            Phase::EvaluationStartedAfterApproval,
        ))
        .await;
    info!(
        %approval,
        pr_number,
        maintainer = maintainer_login,
        "A maintainer approved the commit in a fork pull request."
    );
    true
}

async fn record_approval_approved_without_evaluation_when_no_path_filter_match(
    ci: &CiContext,
    task: &MTask,
    trigger: &MTaskTrigger,
    approval: PullRequestApprovalId,
    maintainer_login: &str,
) -> bool {
    let approved = match approvals::mark_pull_request_approval_approved(
        &ci.db.web_db,
        approval,
        maintainer_login,
    )
    .await
    {
        Ok(Some(approved)) => approved,
        Ok(None) => return false,
        Err(e) => {
            warn!(error = %e, %approval, "Mark fork pull request approval as approved");
            return false;
        }
    };
    touch_trigger_last_fired(&ci.db.web_db, trigger).await;
    ci.db
        .record(approval_event(
            &approved,
            task.project,
            Phase::ApprovalWithoutChangedFileMatchingTriggerPath,
        ))
        .await;
    info!(
        %approval,
        task_id = %task.id,
        maintainer = maintainer_login,
        reason = ?TaskEvaluationNotStartedReason::NoChangedFileMatchesPathFilter,
        "A maintainer approved the commit on a pull request from a fork. The changed files don't match the path filter for triggers. The evaluation doesn't start."
    );
    true
}

/// An approval row can remain pending. The same transaction must also create the evaluation. The only exception is a path filter of the trigger with no matching changed file. The caller can then mark the row approved without an evaluation.
async fn approve_pending_fork_pull_request_approval_and_apply_trigger_in_transaction(
    db: &WebDb,
    task: &MTask,
    approval: PullRequestApprovalId,
    maintainer_login: &str,
    input: ApplyInput,
) -> Result<Option<(MForkPullRequestApproval, ApplyOutcome)>, ApplyError> {
    let transaction = db.begin().await?;
    let Some(approved) =
        approvals::mark_pull_request_approval_approved(&transaction, approval, maintainer_login)
            .await?
    else {
        return Ok(None);
    };
    let applied = apply_trigger(&transaction, task, input).await?;
    if matches!(applied, ApplyOutcome::Created { .. }) {
        transaction.commit().await?;
    }
    Ok(Some((approved, applied)))
}

async fn approve_pending_fork_pull_request_approvals_for_reviewed_commit_and_start_commit_evaluations(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    task_ids: &[TaskId],
    pr_number: u64,
    commit_hash: &[u8],
    maintainer_login: &str,
) {
    for task_id in task_ids {
        match approvals::get_pending_pull_request_approval_for_task_commit(
            &ci.db.web_db,
            *task_id,
            pr_number,
            commit_hash,
        )
        .await
        {
            Ok(Some(pending)) => {
                approve_approval_and_start_approved_commit_evaluation(
                    ci,
                    scheduler,
                    pending.id,
                    maintainer_login,
                    None,
                    None,
                )
                .await;
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, %task_id, pr_number, "Loading Pending Approvals for Fork Pull Requests");
            }
        }
    }
}

pub(super) async fn handle_github_check_run(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
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
    let Some(approval) = approvals::find_approval_by_check_run_id(&ci.db.web_db, check_id)
        .await
        .ok()
        .flatten()
    else {
        warn!(
            check_run_id = check_id,
            "GitHub check_run.requested_action: no fork pull request approval with a matching check run ID"
        );
        return;
    };

    if !login_can_push_to_repository(ci, approval.task, owner, repo, sender.login).await {
        warn!(
            approval_id = %approval.id,
            sender = %sender.login,
            "Rejecting approval click - sender is not a repo writer"
        );
        return;
    }

    approve_approval_and_start_approved_commit_evaluation(
        ci,
        scheduler,
        approval.id,
        sender.login,
        None,
        None,
    )
    .await;
}

/// The trust probe is failing closed on any error.
pub(super) async fn login_can_push_to_repository(
    ci: &CiContext,
    task_id: TaskId,
    owner: &str,
    repo: &str,
    login: &str,
) -> bool {
    let reporter = match gradient_ci::task_commit_status_posting::reporter_for_task(ci, task_id)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return false,
        Err(e) => {
            warn!(error = %e, %task_id, "Resolve the task's reporter for commit statuses for the trust probe.");
            return false;
        }
    };
    match reporter.is_repo_writer(owner, repo, login).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, %task_id, "is_repo_writer probe failed");
            false
        }
    }
}

pub(super) async fn handle_pull_request_review(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    git_host: GitHostType,
    connection_id: Option<GitHostConnectionId>,
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
    let Some(approved_commit_source) = review.approved_commit_source else {
        warn!(
            pr_number,
            "pull_request_review: approval names no reviewed commit and no sender ID, nothing approved"
        );
        return;
    };

    let connection_ids: Vec<GitHostConnectionId> = match connection_id {
        Some(id) => vec![id],
        None => {
            let Some(installation_id) = github_installation_id_from_comment_body(body) else {
                warn!("pull_request_review (github): no installation_id");
                return;
            };
            let targets =
                resolve_github_app_targets(&ci.db.web_db, installation_id, client_ip).await;
            if targets.is_empty() {
                warn!(installation_id, %owner_repo, "pull_request_review (github): No connection to the Git host for the installation");
                return;
            }
            targets
        }
    };

    for connection_id in &connection_ids {
        let task_ids = match load_active_pull_request_tasks_for_event_repository(
            &ci.db.web_db,
            *connection_id,
            owner,
            repo,
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "pull_request_review: failed to load task list");
                continue;
            }
        };
        let Some(probe_task) = first_task_with_reporter(ci, &task_ids).await else {
            continue;
        };
        let reviewer_push_check_and_approved_commit_read_future = async {
            if !login_can_push_to_repository(ci, probe_task, owner, repo, &reviewer).await {
                warn!(
                    %connection_id,
                    pr_number,
                    %reviewer,
                    "Ignoring PR review approval - reviewer is not a repo writer"
                );
                return None;
            }
            match &approved_commit_source {
                ApprovedCommitSource::CommitId(commit) => Some(commit.clone()),
                ApprovedCommitSource::ReviewSenderUserIdOnly { sender_user_id } => {
                    load_sender_newest_undismissed_approving_review_commit(
                        ci,
                        probe_task,
                        owner,
                        repo,
                        pr_number,
                        *sender_user_id,
                        &reviewer,
                    )
                    .await
                }
            }
        };
        let reviewed_commit = match tokio::time::timeout(
            PULL_REQUEST_REVIEW_WEBHOOK_REVIEWER_PUSH_CHECK_AND_APPROVED_COMMIT_READ_TIME_LIMIT,
            reviewer_push_check_and_approved_commit_read_future,
        )
        .await
        {
            Ok(Some(commit)) => commit,
            Ok(None) => continue,
            Err(_) => {
                warn!(
                    %connection_id,
                    pr_number,
                    "pull_request_review: The Git host didn't answer the reviewer push permission check and the review API within the 4 second time limit, so no approval happened"
                );
                continue;
            }
        };
        approve_pending_fork_pull_request_approvals_for_reviewed_commit_and_start_commit_evaluations(
            ci,
            scheduler,
            &task_ids,
            pr_number,
            &reviewed_commit,
            &reviewer,
        )
        .await;
    }
}

async fn load_sender_newest_undismissed_approving_review_commit(
    ci: &CiContext,
    task_id: TaskId,
    owner: &str,
    repo: &str,
    pr_number: u64,
    sender_user_id: i64,
    webhook_sender_login: &str,
) -> Option<Vec<u8>> {
    let lookup = async {
        let Some(reporter) =
            gradient_ci::task_commit_status_posting::reporter_for_task(ci, task_id).await?
        else {
            return Ok(None);
        };
        reporter
            .get_newest_undismissed_approval_review_commit_id_for_user(
                owner,
                repo,
                pr_number,
                sender_user_id,
                webhook_sender_login,
            )
            .await
    };
    match lookup.await {
        Ok(Some(commit)) => Some(commit),
        Ok(None) => {
            info!(
                %task_id,
                pr_number,
                reviewer = webhook_sender_login,
                "pull_request_review: git host did not specify approved commits for sender (no matching reviews, 404, or page limit), so there are no approved commits"
            );
            None
        }
        Err(e) => {
            warn!(
                error = %e,
                %task_id,
                pr_number,
                "pull_request_review: failed to read the commit under review from the review API of the git host, so no approval happened"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::{project_id, task_id, user_id};
    use gradient_fixtures::{rows, test_date};
    use gradient_test_support::git_daemon_and_fork_repository_fixtures::{
        create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash,
        start_git_daemon_on_free_loopback_port_serving_base_directory,
    };
    use gradient_test_support::state::TestState;
    use gradient_types::triggers::{ChangedFilePathFilter, TriggerConfig};
    use sea_orm::DatabaseConnection;

    #[tokio::test]
    async fn approving_pending_fork_pull_request_approval_starts_one_approved_commit_evaluation_and_approving_again_or_approving_next_commit_during_evaluation_starts_no_evaluation_and_leaves_next_approval_pending()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        gradient_db::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the user is written");
        gradient_db::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        gradient_db::rows::insert::<ETask, _>(&conn, rows::task())
            .await
            .expect("the task is written");
        gradient_db::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        gradient_db::rows::insert::<EProjectCache, _>(
            &conn,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project_id(),
                rows::cache().id,
                gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
            ),
        )
        .await
        .expect("the project writes to the cache");
        gradient_db::rows::insert::<EWorkerRegistration, _>(
            &conn,
            MWorkerRegistration {
                peer_id: project_id(),
                created_by: Some(user_id()),
                ..rows::worker_registration(true, true)
            },
        )
        .await
        .expect("the evaluating worker is registered");
        let pull_request = TriggerConfig::ReporterPullRequest {
            branches: vec![],
            actions: vec![],
            require_approval: true,
            path_filter: ChangedFilePathFilter::default(),
        };
        let trigger = gradient_db::triggers::insert(
            &conn,
            MTaskTrigger {
                id: TaskTriggerId::now_v7(),
                task: task_id(),
                trigger_type: pull_request.trigger_type(),
                config: pull_request.to_db_json(),
                active: true,
                created_at: test_date(),
                updated_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the trigger is written");
        let pending = gradient_db::rows::insert::<EForkPullRequestApproval, _>(
            &conn,
            MForkPullRequestApproval {
                id: PullRequestApprovalId::now_v7(),
                task: task_id(),
                trigger: trigger.id,
                pr_number: 7,
                pr_author: "external".into(),
                commit_hash: vec![9; 20],
                head_repo_clone_url: Some("https://github.com/external/repo".into()),
                created_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the pending approval is written");
        let state = TestState::new()
            .web_db(conn.clone())
            .worker_db(conn)
            .build();
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let ci = state.ci();

        let first = approve_approval_and_start_approved_commit_evaluation(
            &ci,
            &scheduler,
            pending.id,
            "maintainer",
            None,
            None,
        )
        .await;
        let second = approve_approval_and_start_approved_commit_evaluation(
            &ci,
            &scheduler,
            pending.id,
            "maintainer",
            None,
            None,
        )
        .await;

        assert!(first);
        assert!(!second);
        let evaluations = db.rows::<EEvaluation>().await;
        assert_eq!(evaluations.len(), 1);
        assert_eq!(
            (
                evaluations[0].status,
                evaluations[0].allows_project_credentials_and_privileged_actions,
            ),
            (gradient_entity::evaluation::EvaluationStatus::Queued, true)
        );
        assert_eq!(
            evaluations[0].repository,
            "https://github.com/external/repo"
        );
        let commit = db
            .find::<ECommit>(evaluations[0].commit)
            .await
            .expect("the evaluated commit");
        assert_eq!(commit.hash, vec![9; 20]);
        let approved = db
            .find::<EForkPullRequestApproval>(pending.id)
            .await
            .expect("the approval");
        assert_eq!(
            approved.approving_maintainer_login.as_deref(),
            Some("maintainer")
        );

        for status in [
            gradient_entity::evaluation::EvaluationStatus::Fetching,
            gradient_entity::evaluation::EvaluationStatus::EvaluatingFlake,
            gradient_entity::evaluation::EvaluationStatus::EvaluatingDerivation,
            gradient_entity::evaluation::EvaluationStatus::Building,
        ] {
            gradient_db::status::change_evaluation_status(
                &db.connection(),
                evaluations[0].id,
                status.report(),
                |update| update,
            )
            .await
            .expect("Write the evaluation status")
            .expect("Approved evaluations move to the next status step in the scheduler.");
        }
        let blocked = gradient_db::rows::insert::<EForkPullRequestApproval, _>(
            &db.connection(),
            MForkPullRequestApproval {
                id: PullRequestApprovalId::now_v7(),
                task: task_id(),
                trigger: trigger.id,
                pr_number: 7,
                pr_author: "external".into(),
                commit_hash: vec![8; 20],
                created_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the pending approval of the next commit is written");

        let skipped = approve_approval_and_start_approved_commit_evaluation(
            &ci,
            &scheduler,
            blocked.id,
            "maintainer",
            None,
            None,
        )
        .await;

        assert!(!skipped);
        assert_eq!(db.rows::<EEvaluation>().await.len(), 1);
        let still_pending = db
            .find::<EForkPullRequestApproval>(blocked.id)
            .await
            .expect("the approval");
        assert!(still_pending.approved_at.is_none());
        assert!(still_pending.approving_maintainer_login.is_none());
        db.finish().await;
    }

    async fn write_user_project_task_cache_and_evaluating_worker(
        conn: &DatabaseConnection,
        git_server_url: &str,
    ) {
        gradient_db::rows::insert::<EUser, _>(conn, rows::user())
            .await
            .expect("the user is written");
        gradient_db::rows::insert::<EProject, _>(conn, rows::project())
            .await
            .expect("the project is written");
        gradient_db::rows::insert::<ETask, _>(
            conn,
            MTask {
                repository: format!("{git_server_url}/test-org/repo"),
                ..rows::task()
            },
        )
        .await
        .expect("the task is written");
        gradient_db::rows::insert::<ECache, _>(conn, rows::cache())
            .await
            .expect("the cache is written");
        gradient_db::rows::insert::<EProjectCache, _>(
            conn,
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project_id(),
                rows::cache().id,
                gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
            ),
        )
        .await
        .expect("the project writes to the cache");
        gradient_db::rows::insert::<EWorkerRegistration, _>(
            conn,
            MWorkerRegistration {
                peer_id: project_id(),
                created_by: Some(user_id()),
                ..rows::worker_registration(true, true)
            },
        )
        .await
        .expect("the evaluating worker is registered");
    }

    async fn create_pending_fork_pull_request_approval(
        conn: &DatabaseConnection,
        task: TaskId,
        git_server_url: &str,
        include_path: &str,
        head_commit_hash: &str,
    ) -> MForkPullRequestApproval {
        let pull_request = TriggerConfig::ReporterPullRequest {
            branches: vec![],
            actions: vec![],
            require_approval: true,
            path_filter: ChangedFilePathFilter {
                include_paths: vec![include_path.to_owned()],
                exclude_paths: vec![],
            },
        };
        let trigger = gradient_db::triggers::insert(
            conn,
            MTaskTrigger {
                id: TaskTriggerId::now_v7(),
                task,
                trigger_type: pull_request.trigger_type(),
                config: pull_request.to_db_json(),
                active: true,
                created_at: test_date(),
                updated_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the trigger is written");
        gradient_db::rows::insert::<EForkPullRequestApproval, _>(
            conn,
            MForkPullRequestApproval {
                id: PullRequestApprovalId::now_v7(),
                task,
                trigger: trigger.id,
                pr_number: 7,
                pr_author: "contributor".into(),
                commit_hash: hex::decode(head_commit_hash).expect("a hex commit hash"),
                head_repo_clone_url: Some(format!("{git_server_url}/external/repo")),
                pr_base_branch_name: Some("main".into()),
                created_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the pending approval is written")
    }

    async fn write_second_project_with_pending_approval_for_same_commit(
        conn: &DatabaseConnection,
        git_server_url: &str,
        head_commit_hash: &str,
    ) -> MForkPullRequestApproval {
        let project = gradient_db::rows::insert::<EProject, _>(
            conn,
            rows::project_with_id(ProjectId::now_v7(), "second-project"),
        )
        .await
        .expect("the second project is written");
        let task = gradient_db::rows::insert::<ETask, _>(
            conn,
            MTask {
                id: TaskId::now_v7(),
                project: project.id,
                repository: format!("{git_server_url}/test-org/repo"),
                ..rows::task()
            },
        )
        .await
        .expect("the task of the second project is written");
        create_pending_fork_pull_request_approval(
            conn,
            task.id,
            git_server_url,
            "src/*",
            head_commit_hash,
        )
        .await
    }

    fn build_test_ci_context_and_scheduler_from_db_connection(
        conn: DatabaseConnection,
    ) -> (CiContext, Arc<Scheduler>) {
        let state = TestState::new()
            .web_db(conn.clone())
            .worker_db(conn)
            .build();
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        (state.ci(), scheduler)
    }

    async fn pending_delivery_event_phases(db: &TestDatabase) -> Vec<serde_json::Value> {
        db.rows::<EPendingDelivery>()
            .await
            .into_iter()
            .filter_map(|delivery| {
                delivery
                    .payload
                    .pointer("/ForkPullRequestApprovalReported/phase")
                    .cloned()
            })
            .collect()
    }

    #[tokio::test]
    async fn approving_fork_pull_request_commit_skips_approved_commit_evaluation_when_path_filter_finds_no_changed_file_matching_stored_base_branch_and_marks_approval_approved()
     {
        let served = tempfile::tempdir().expect("a temporary directory");
        let head = create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash(
            served.path(),
            "src/main.rs",
            "docs/guide.md",
        );
        let (_daemon, git_server_url) =
            start_git_daemon_on_free_loopback_port_serving_base_directory(served.path());
        let db = TestDatabase::create().await;
        let conn = db.connection();
        write_user_project_task_cache_and_evaluating_worker(&conn, &git_server_url).await;
        let pending = create_pending_fork_pull_request_approval(
            &conn,
            task_id(),
            &git_server_url,
            "src/*",
            &head,
        )
        .await;
        let (ci, scheduler) = build_test_ci_context_and_scheduler_from_db_connection(conn);

        let recorded = approve_approval_and_start_approved_commit_evaluation(
            &ci,
            &scheduler,
            pending.id,
            "maintainer",
            None,
            None,
        )
        .await;

        assert!(recorded);
        assert!(db.rows::<EEvaluation>().await.is_empty());
        let approved = db
            .find::<EForkPullRequestApproval>(pending.id)
            .await
            .expect("the approval");
        assert_eq!(
            approved.approving_maintainer_login.as_deref(),
            Some("maintainer")
        );
        assert!(
            pending_delivery_event_phases(&db)
                .await
                .contains(&serde_json::json!(
                    "approval_without_changed_file_matching_trigger_path"
                ))
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn approving_fork_pull_request_commit_that_changes_matching_file_starts_one_evaluation() {
        let served = tempfile::tempdir().expect("a temporary directory");
        let head = create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash(
            served.path(),
            "src/main.rs",
            "docs/guide.md",
        );
        let (_daemon, git_server_url) =
            start_git_daemon_on_free_loopback_port_serving_base_directory(served.path());
        let db = TestDatabase::create().await;
        let conn = db.connection();
        write_user_project_task_cache_and_evaluating_worker(&conn, &git_server_url).await;
        let pending = create_pending_fork_pull_request_approval(
            &conn,
            task_id(),
            &git_server_url,
            "docs/*",
            &head,
        )
        .await;
        let (ci, scheduler) = build_test_ci_context_and_scheduler_from_db_connection(conn);

        let started = approve_approval_and_start_approved_commit_evaluation(
            &ci,
            &scheduler,
            pending.id,
            "maintainer",
            None,
            None,
        )
        .await;

        assert!(started);
        assert_eq!(db.rows::<EEvaluation>().await.len(), 1);
        db.finish().await;
    }

    #[tokio::test]
    async fn changed_file_list_from_path_filter_is_not_reused_for_other_project() {
        let served = tempfile::tempdir().expect("a temporary directory");
        let head = create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash(
            served.path(),
            "src/main.rs",
            "docs/guide.md",
        );
        let (_daemon, git_server_url) =
            start_git_daemon_on_free_loopback_port_serving_base_directory(served.path());
        let db = TestDatabase::create().await;
        let conn = db.connection();
        write_user_project_task_cache_and_evaluating_worker(&conn, &git_server_url).await;
        let pending = create_pending_fork_pull_request_approval(
            &conn,
            task_id(),
            &git_server_url,
            "src/*",
            &head,
        )
        .await;
        let second_project_task_pending_approval_row =
            write_second_project_with_pending_approval_for_same_commit(
                &conn,
                &git_server_url,
                &head,
            )
            .await;
        let (ci, scheduler) = build_test_ci_context_and_scheduler_from_db_connection(conn);
        let second_project_approval_recorded =
            approve_approval_and_start_approved_commit_evaluation(
                &ci,
                &scheduler,
                second_project_task_pending_approval_row.id,
                "maintainer",
                None,
                None,
            )
            .await;
        std::fs::rename(
            served.path().join("test-org/repo"),
            served.path().join("test-org/removed"),
        )
        .expect("the base repository is moved away");

        let started = approve_approval_and_start_approved_commit_evaluation(
            &ci,
            &scheduler,
            pending.id,
            "maintainer",
            None,
            None,
        )
        .await;

        assert!(second_project_approval_recorded && started);
        assert_eq!(db.rows::<EEvaluation>().await.len(), 1);
        db.finish().await;
    }
}
