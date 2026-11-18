/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::commands::{
    first_task_with_reporter, github_installation_id_from_comment_body,
    load_active_pull_request_tasks_for_event_repository,
};
use super::fanout::{project_name_for, record_trigger_apply_outcome, touch_trigger_last_fired};
use super::installation::{resolve_github_app_targets, task_serves_repository};
use super::payloads::GithubCheckRunRequestedAction;
use super::response::WebhookTriggerOutcome;
use gradient_ci::CiContext;
use gradient_ci::{APPROVAL_ACTION_ID, ApplyError, ApplyInput, ApplyOutcome, apply_trigger};
use gradient_db::fork_pull_request_approvals as approvals;
use gradient_db::lookup::by_id;
use gradient_db::{DbContext, WebDb};
use gradient_entity::evaluation::WalkMode;
use gradient_git_host::{ParsedPullRequestEvent, ParsedPullRequestReviewEvent};
use gradient_scheduler::Scheduler;
use gradient_types::branch_key;
use gradient_types::events::fork_pull_request_approval_events::{Phase, Reported};
use gradient_types::input::vec_to_hex;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::TransactionTrait;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub(super) struct PullRequestApprovalContext {
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub is_fork: Option<bool>,
    pub sender: Option<String>,
}

impl From<&ParsedPullRequestEvent> for PullRequestApprovalContext {
    fn from(parsed: &ParsedPullRequestEvent) -> Self {
        Self {
            pr_number: parsed.pr_number,
            pr_author: parsed.pr_author.clone(),
            is_fork: parsed.is_fork,
            sender: parsed.sender.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ApprovalInfo {
    pub pr_number: u64,
    pub pr_author: String,
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
    let stored =
        approvals::add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
            &db.web_db, pending,
        )
        .await?;
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
        repository_override: pending.head_repo_clone_url,
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

/// Approval rows stay pending unless the same transaction can also create an evaluation.
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

pub(super) async fn approve_newest_pending_approval_and_start_its_evaluation(
    ci: &CiContext,
    scheduler: &Arc<Scheduler>,
    task_ids: &[TaskId],
    pr_number: u64,
    maintainer_login: &str,
    wildcard_override: &Option<String>,
    source_comment: Option<serde_json::Value>,
) -> bool {
    let mut started_any = false;
    for task_id in task_ids {
        let pending = match approvals::get_newest_pending_pull_request_approval_for_task(
            &ci.db.web_db,
            *task_id,
            pr_number,
        )
        .await
        {
            Ok(Some(pending)) => pending,
            Ok(None) => continue,
            Err(e) => {
                warn!(error = %e, %task_id, pr_number, "Loading Pending Approvals for Fork Pull Requests");
                continue;
            }
        };
        started_any |= approve_approval_and_start_approved_commit_evaluation(
            ci,
            scheduler,
            pending.id,
            maintainer_login,
            wildcard_override.clone(),
            source_comment.clone(),
        )
        .await;
    }
    started_any
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
    let task_repository = by_id::<ETask, _>(&ci.db.web_db, approval.task)
        .await
        .ok()
        .flatten()
        .map(|task| task.repository);
    if !task_repository.is_some_and(|url| task_serves_repository(&url, owner, repo)) {
        warn!(
            approval_id = %approval.id,
            %full_name,
            "Reject approval click - this approval is for a different repository"
        );
        return;
    }

    if !sender_is_trusted(ci, approval.task, owner, repo, sender.login).await {
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
pub(super) async fn sender_is_trusted(
    ci: &CiContext,
    task_id: TaskId,
    owner: &str,
    repo: &str,
    sender: &str,
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
    match reporter.is_repo_writer(owner, repo, sender).await {
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
    let Some(commit_hash) = review.commit_hash else {
        warn!(
            pr_number,
            "pull_request_review: approval without a reviewed commit"
        );
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
        if !sender_is_trusted(ci, probe_task, owner, repo, &reviewer).await {
            warn!(
                %connection_id,
                pr_number,
                %reviewer,
                "Ignoring PR review approval - reviewer is not a repo writer"
            );
            continue;
        }
        approve_pending_fork_pull_request_approvals_for_reviewed_commit_and_start_commit_evaluations(
            ci,
            scheduler,
            &task_ids,
            pr_number,
            &commit_hash,
            &reviewer,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::task_id;
    use gradient_fixtures::{rows, test_date};
    use gradient_test_support::state::TestState;
    use gradient_types::triggers::{ChangedFilePathFilter, TriggerConfig};

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

        gradient_db::status::change_evaluation_status(
            &db.connection(),
            evaluations[0].id,
            gradient_entity::evaluation::EvaluationStatus::Building.report(),
            |update| update,
        )
        .await
        .expect("the evaluation builds")
        .expect("the parked evaluation can start building");
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
}
