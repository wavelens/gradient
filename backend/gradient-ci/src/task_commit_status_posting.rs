/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The task can post commit statuses only with report_status on and status reporting enabled on the connection to the Git host. The task can omit statuses for input_update evaluations. The commit for an input_update evaluation is empty before the push of the pull request.

use crate::actions::report::{
    build_ci_reports_from_commit_status_check_rows, persist_evaluation_check_id,
};
use crate::context::CiContext;
use crate::git_host_connection_token_refresh::refresh_and_get_usable_decrypted_access_token_for_task_repository;
use crate::reporter_restricted_to_granted_repository::ReporterRestrictedToGrantedRepository;
use crate::reporting::CommitStatusCheckWithIds;
use anyhow::{Context, Result, anyhow};
use gradient_db::git_host_connection_repository_grants::{
    GitHostConnectionRepositoryAccessProof, GitHostConnectionRepositoryGrantError,
    load_task_git_host_connection_project_and_grants_and_check_repository_access,
    load_task_project_and_grants_and_check_given_git_host_connection_for_task_repository_access,
};
use gradient_db::lookup::by_id;
use gradient_entity::task_commit_status_post_attempt as post_attempt;
use gradient_git_host::reporter::{CiReport, CiReporter, CiStatus, GithubAppReporter};
use gradient_types::ids::TaskCommitStatusPostAttemptId;
use gradient_types::*;
use std::sync::Arc;
use std::time::Instant;
use tracing::warn;

#[derive(Debug, thiserror::Error)]
pub enum CommitStatusTestSendError {
    #[error("No connection to a git host for this task")]
    NoConnection,
    #[error("The connection {name} to the Git host doesn't report commit statuses")]
    ConnectionCannotReportStatus { name: String },
    #[error("This task has no evaluated commit yet. Run an evaluation first.")]
    NoEvaluatedCommit,
    #[error("Can't read owner and repository from {repository}")]
    RepositoryUnparsable { repository: String },
    #[error("{0}")]
    GitHostConnectionCannotActOnTaskRepository(GitHostConnectionRepositoryGrantError),
}

pub async fn load_task_git_host_connection(
    ctx: &CiContext,
    task_id: TaskId,
) -> Result<Option<(MTask, GitHostConnectionRepositoryAccessProof)>> {
    let Some(task) = by_id::<ETask, _>(&ctx.db.worker_db, task_id)
        .await
        .context("load task")?
    else {
        return Ok(None);
    };
    match load_task_git_host_connection_project_and_grants_and_check_repository_access(
        &ctx.db.worker_db,
        &task,
    )
    .await
    .context("check the git host connection of the task")?
    {
        Ok(allowed) => Ok(Some((task, allowed))),
        Err(GitHostConnectionRepositoryGrantError::NoConnection) => Ok(None),
        Err(refusal) => {
            warn!(task = %task.id, %refusal, "connection to the Git host has no permission for the task repository");
            Ok(None)
        }
    }
}

pub fn task_should_post_commit_statuses(task: &MTask, connection: &MGitHostConnection) -> bool {
    task.report_status && connection.reports_status
}

pub async fn reporter_for_task(
    ctx: &CiContext,
    task_id: TaskId,
) -> Result<Option<Arc<dyn CiReporter>>> {
    match load_task_git_host_connection(ctx, task_id).await? {
        Some((task, allowed)) if task_should_post_commit_statuses(&task, allowed.connection()) => {
            Ok(Some(
                build_git_host_reporter_refusing_other_repositories(ctx, &allowed).await?,
            ))
        }
        _ => Ok(None),
    }
}

pub(crate) async fn build_git_host_reporter_refusing_other_repositories(
    ctx: &CiContext,
    allowed: &GitHostConnectionRepositoryAccessProof,
) -> Result<Arc<dyn CiReporter>> {
    let reporter =
        build_unrestricted_git_host_reporter_for_allowed_connection(ctx, allowed).await?;
    Ok(Arc::new(ReporterRestrictedToGrantedRepository::new(
        reporter, allowed,
    )))
}

pub(crate) async fn build_task_repository_reporter_if_git_host_connection_allowed(
    ctx: &CiContext,
    task: &MTask,
    connection: GitHostConnectionId,
) -> Result<Result<Arc<dyn CiReporter>, GitHostConnectionRepositoryGrantError>> {
    let allowed = load_task_project_and_grants_and_check_given_git_host_connection_for_task_repository_access(
        &ctx.db.worker_db,
        task,
        connection,
    )
    .await
    .context("check the git host connection of the task")?;
    match allowed {
        Ok(allowed) => Ok(Ok(build_git_host_reporter_refusing_other_repositories(
            ctx, &allowed,
        )
        .await?)),
        Err(refusal) => Ok(Err(refusal)),
    }
}

async fn build_unrestricted_git_host_reporter_for_allowed_connection(
    ctx: &CiContext,
    allowed: &GitHostConnectionRepositoryAccessProof,
) -> Result<Arc<dyn CiReporter>> {
    let connection = allowed.connection();
    let git_host = connection.git_host_type;
    let provider = ctx
        .git_host
        .get(git_host)
        .ok_or_else(|| anyhow!("no Git host provider registered for {:?}", git_host))?;

    if provider.supports_app_auth()
        && let Some(installation_id) = connection.github_installation_id
        && let Some(github_app) = ctx.db.config.github_app.clone()
    {
        let pem = tokio::fs::read_to_string(&github_app.private_key_file)
            .await
            .context("read github app private key")?;
        let reporter = GithubAppReporter::new(
            ctx.db.user_supplied_url_http_client.clone(),
            "",
            github_app.app_id,
            pem,
            installation_id,
        )?;
        return Ok(Arc::new(reporter));
    }

    let token = refresh_and_get_usable_decrypted_access_token_for_task_repository(
        ctx.db.web_db.inner(),
        &ctx.db.config,
        &ctx.db.user_supplied_url_http_client,
        allowed,
    )
    .await?;
    provider.build_reporter(
        ctx.db.user_supplied_url_http_client.clone(),
        gradient_types::git_host::base_url_of_git_host_connection(connection),
        token.as_ref().map(|t| t.expose()),
    )
}

struct PostOutcome {
    response_status: Option<i32>,
    response_body: Option<String>,
}

async fn post_reports(
    ctx: &CiContext,
    reporter: &dyn CiReporter,
    reports: &[CiReport],
    check: Option<&CommitStatusCheckWithIds>,
) -> Result<PostOutcome> {
    let mut first_id = None;
    for report in reports {
        let new_id = reporter
            .report(report)
            .await
            .context("send Git host status report")?;
        if let (Some(new_id), Some(check)) = (new_id, check) {
            persist_check_run_id(ctx, check, &report.context, new_id).await?;
        }
        first_id = first_id.or(new_id);
    }
    Ok(PostOutcome {
        response_status: Some(200),
        response_body: first_id.map(|id| format!("{{\"check_run_id\":{id}}}")),
    })
}

async fn persist_check_run_id(
    ctx: &CiContext,
    check: &CommitStatusCheckWithIds,
    context: &str,
    check_run_id: i64,
) -> Result<()> {
    match check {
        CommitStatusCheckWithIds::Approval { approval, .. } => {
            gradient_db::fork_pull_request_approvals::store_approval_check_run_id(
                &ctx.db.worker_db,
                *approval,
                check_run_id,
            )
            .await
            .context("store the check run id of the approval check")?;
        }
        other => {
            if let Some(evaluation_id) = other.evaluation_id() {
                persist_evaluation_check_id(ctx, evaluation_id, context, check_run_id).await;
            }
        }
    }
    Ok(())
}

async fn record_attempt(
    ctx: &CiContext,
    task: TaskId,
    connection: GitHostConnectionId,
    event: &str,
    started: Instant,
    outcome: &Result<PostOutcome>,
) -> Result<post_attempt::Model> {
    let (response_status, response_body, error_message) = match outcome {
        Ok(ok) => (ok.response_status, ok.response_body.clone(), None),
        Err(e) => (None, None, Some(format!("{e:#}"))),
    };
    let attempt = post_attempt::Model {
        id: TaskCommitStatusPostAttemptId::now_v7(),
        task,
        git_host_connection: Some(connection),
        event: event.to_owned(),
        response_status,
        response_body,
        error_message,
        success: outcome.is_ok(),
        duration_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
        delivered_at: chrono::Utc::now().naive_utc(),
    };
    gradient_db::git_host_connections::record_commit_status_post_attempt(
        &ctx.db.worker_db,
        attempt.clone(),
    )
    .await
    .context("record commit status post attempt")?;
    Ok(attempt)
}

pub async fn post_commit_status_reports_for_task_check_event_and_record_attempt(
    ctx: &CiContext,
    task_id: TaskId,
    event: &str,
    check: &CommitStatusCheckWithIds,
) -> Result<()> {
    let Some((task, allowed)) = load_task_git_host_connection(ctx, task_id).await? else {
        return Ok(());
    };
    let connection = allowed.connection();
    if !task_should_post_commit_statuses(&task, connection) {
        return Ok(());
    }
    let reports = build_ci_reports_from_commit_status_check_rows(ctx, check).await?;
    if reports.is_empty() {
        return Ok(());
    }

    let started = Instant::now();
    let outcome = async {
        let reporter = build_git_host_reporter_refusing_other_repositories(ctx, &allowed).await?;
        post_reports(ctx, reporter.as_ref(), &reports, Some(check)).await
    }
    .await;
    record_attempt(ctx, task.id, connection.id, event, started, &outcome).await?;
    outcome.map(|_| ())
}

pub const USER_TRIGGERED_TEST_STATUS_POST_EVENT_NAME: &str = "test.commit_status";

pub async fn post_pending_test_commit_status_for_last_evaluation_and_record_attempt(
    ctx: &CiContext,
    task: &MTask,
) -> Result<post_attempt::Model> {
    let allowed =
        match load_task_git_host_connection_project_and_grants_and_check_repository_access(
            &ctx.db.worker_db,
            task,
        )
        .await
        .context("check the git host connection of the task")?
        {
            Ok(allowed) => allowed,
            Err(GitHostConnectionRepositoryGrantError::NoConnection) => {
                return Err(CommitStatusTestSendError::NoConnection.into());
            }
            Err(refusal) => {
                return Err(
                    CommitStatusTestSendError::GitHostConnectionCannotActOnTaskRepository(refusal)
                        .into(),
                );
            }
        };
    let connection = allowed.connection();
    if !connection.reports_status {
        return Err(CommitStatusTestSendError::ConnectionCannotReportStatus {
            name: connection.name.clone(),
        }
        .into());
    }
    let evaluation_id = task
        .last_evaluation
        .ok_or(CommitStatusTestSendError::NoEvaluatedCommit)?;
    let evaluation = by_id::<EEvaluation, _>(&ctx.db.worker_db, evaluation_id)
        .await
        .context("load evaluation")?
        .ok_or(CommitStatusTestSendError::NoEvaluatedCommit)?;
    let commit = by_id::<ECommit, _>(&ctx.db.worker_db, evaluation.commit)
        .await
        .context("load commit")?
        .filter(|c| !c.hash.is_empty())
        .ok_or(CommitStatusTestSendError::NoEvaluatedCommit)?;
    let (owner, repo) = crate::parse_owner_repo(&task.repository).ok_or_else(|| {
        CommitStatusTestSendError::RepositoryUnparsable {
            repository: task.repository.clone(),
        }
    })?;
    let report = CiReport {
        owner,
        repo,
        sha: hex::encode(&commit.hash),
        context: format!("gradient/{}: Status test", task.name),
        status: CiStatus::Pending,
        description: Some("Test for status reports on commits from Gradient".to_owned()),
        details_url: None,
        existing_check_id: None,
        requested_actions: Vec::new(),
    };

    let started = Instant::now();
    let outcome = async {
        let reporter = build_git_host_reporter_refusing_other_repositories(ctx, &allowed).await?;
        post_reports(ctx, reporter.as_ref(), &[report], None).await
    }
    .await;
    record_attempt(
        ctx,
        task.id,
        connection.id,
        USER_TRIGGERED_TEST_STATUS_POST_EVENT_NAME,
        started,
        &outcome,
    )
    .await
}

#[cfg(test)]
mod tests;
