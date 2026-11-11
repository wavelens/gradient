/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The task can post commit statuses only with report_status on and status reporting enabled on the connection to the Git host. The task can omit statuses for input_update evaluations. The commit for an input_update evaluation is empty before the push of the pull request.

use crate::actions::git_host_status_for_event;
use crate::actions::report::{build_ci_reports_from_payload, persist_evaluation_check_id};
use crate::context::CiContext;
use crate::git_host_connection_token_refresh::refresh_and_get_usable_decrypted_access_token;
use anyhow::{Context, Result, anyhow};
use gradient_db::lookup::by_id;
use gradient_entity::task_commit_status_post_attempt as post_attempt;
use gradient_git_host::reporter::{CiReport, CiReporter, CiStatus, GithubAppReporter};
use gradient_types::ids::TaskCommitStatusPostAttemptId;
use gradient_types::*;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use std::time::Instant;

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
}

pub async fn load_task_git_host_connection(
    ctx: &CiContext,
    task_id: TaskId,
) -> Result<Option<(MTask, MGitHostConnection)>> {
    let Some(task) = by_id::<ETask, _>(&ctx.db.worker_db, task_id)
        .await
        .context("load task")?
    else {
        return Ok(None);
    };
    let Some(connection_id) = task.git_host_connection else {
        return Ok(None);
    };
    let connection = by_id::<EGitHostConnection, _>(&ctx.db.worker_db, connection_id)
        .await
        .context("load git host connection")?;
    Ok(connection.map(|connection| (task, connection)))
}

pub fn task_should_post_commit_statuses(task: &MTask, connection: &MGitHostConnection) -> bool {
    task.report_status && connection.reports_status
}

pub async fn reporter_for_task(
    ctx: &CiContext,
    task_id: TaskId,
) -> Result<Option<Arc<dyn CiReporter>>> {
    match load_task_git_host_connection(ctx, task_id).await? {
        Some((task, connection)) if task_should_post_commit_statuses(&task, &connection) => {
            Ok(Some(
                build_commit_status_reporter_from_git_host_connection_row(ctx, &connection).await?,
            ))
        }
        _ => Ok(None),
    }
}

pub async fn build_commit_status_posting_client_from_git_host_connection_id(
    ctx: &CiContext,
    connection_id: GitHostConnectionId,
) -> Result<Arc<dyn CiReporter>> {
    let connection = by_id::<EGitHostConnection, _>(&ctx.db.worker_db, connection_id)
        .await
        .context("load git host connection")?
        .ok_or_else(|| anyhow!("git host connection {connection_id} not found"))?;
    build_commit_status_reporter_from_git_host_connection_row(ctx, &connection).await
}

pub async fn build_commit_status_reporter_from_git_host_connection_row(
    ctx: &CiContext,
    connection: &MGitHostConnection,
) -> Result<Arc<dyn CiReporter>> {
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
            ctx.http.clone(),
            "",
            github_app.app_id,
            pem,
            installation_id,
        )?;
        return Ok(Arc::new(reporter));
    }

    let token = refresh_and_get_usable_decrypted_access_token(
        ctx.db.web_db.inner(),
        &ctx.db.config,
        &ctx.http,
        connection,
    )
    .await?;
    provider.build_reporter(
        ctx.http.clone(),
        connection.base_url.as_deref(),
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
    evaluation_id: Option<EvaluationId>,
) -> Result<PostOutcome> {
    let mut first_id = None;
    for report in reports {
        let new_id = reporter
            .report(report)
            .await
            .context("send Git host status report")?;
        if let (Some(new_id), Some(evaluation_id)) = (new_id, evaluation_id) {
            persist_evaluation_check_id(ctx, evaluation_id, &report.context, new_id).await;
        }
        first_id = first_id.or(new_id);
    }
    Ok(PostOutcome {
        response_status: Some(200),
        response_body: first_id.map(|id| format!("{{\"check_run_id\":{id}}}")),
    })
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

pub async fn post_commit_statuses_for_task_event_and_record_attempt(
    ctx: &CiContext,
    task_id: TaskId,
    event: &str,
    payload: &JsonValue,
) -> Result<()> {
    let Some((task, connection)) = load_task_git_host_connection(ctx, task_id).await? else {
        return Ok(());
    };
    if !task_should_post_commit_statuses(&task, &connection) {
        return Ok(());
    }
    let Some(status) = git_host_status_for_event(event) else {
        return Ok(());
    };
    let reports = build_ci_reports_from_payload(ctx, event, payload, status).await?;
    if reports.is_empty() {
        return Ok(());
    }
    let evaluation_id = payload
        .get("evaluation_id")
        .or_else(|| payload.get("evaluation"))
        .and_then(|v| v.as_str())
        .and_then(|eid| eid.parse::<EvaluationId>().ok());

    let started = Instant::now();
    let outcome = async {
        let reporter =
            build_commit_status_reporter_from_git_host_connection_row(ctx, &connection).await?;
        post_reports(ctx, reporter.as_ref(), &reports, evaluation_id).await
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
    let connection_id = task
        .git_host_connection
        .ok_or(CommitStatusTestSendError::NoConnection)?;
    let connection = by_id::<EGitHostConnection, _>(&ctx.db.worker_db, connection_id)
        .await
        .context("load git host connection")?
        .ok_or(CommitStatusTestSendError::NoConnection)?;
    if !connection.reports_status {
        return Err(CommitStatusTestSendError::ConnectionCannotReportStatus {
            name: connection.name,
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
        let reporter =
            build_commit_status_reporter_from_git_host_connection_row(ctx, &connection).await?;
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
