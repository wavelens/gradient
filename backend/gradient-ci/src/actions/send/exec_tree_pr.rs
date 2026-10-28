/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::open_pr::{has_open_pr, no_op, upsert_open_pr_state};
use crate::ActionError;
use crate::actions::ExecutorOk;
use crate::context::CiContext;
use crate::pull_requests::{
    MAX_PULL_REQUEST_PATCH_BYTES, OpenedPullRequest, PatchPullRequest, PullRequestError,
    evaluation_commit_of_job, open_from_patch,
};
use anyhow::{Context, Result, anyhow};
use gradient_db::exec_jobs::outputs;
use gradient_db::lookup::by_id;
use gradient_types::{ETask, ExecJobId, IntegrationId, TaskActionId, TaskId};
use gradient_wire::types::TREE_PATCH_OUTPUT;
use serde_json::Value as JsonValue;

const DEFAULT_TITLE: &str = "update from {entry_point}";
const DEFAULT_BODY: &str = "Opened by Gradient from exec job {job}.";

#[allow(
    clippy::too_many_arguments,
    reason = "mirrors execute_open_pr; refactor tracked in #503"
)]
pub(crate) async fn execute_exec_tree_pr(
    ctx: &CiContext,
    payload: &JsonValue,
    action_id: TaskActionId,
    task_id: TaskId,
    integration_id: IntegrationId,
    branch_pattern: &str,
    title_template: Option<&str>,
    body_template: Option<&str>,
    update_existing: bool,
) -> Result<ExecutorOk> {
    let job = job_of(payload)?;
    let entry_point = payload
        .get("entry_point_name")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let Some(patch) = tree_patch(ctx, job).await? else {
        return Ok(no_op());
    };

    let branch = branch_pattern.replace("{entry_point}", entry_point);
    if !update_existing && has_open_pr(ctx, action_id, &branch).await? {
        return Ok(no_op());
    }

    let render = |template: Option<&str>, default: &str| {
        template
            .unwrap_or(default)
            .replace("{entry_point}", entry_point)
            .replace("{job}", &job.to_string())
    };
    let request = PatchPullRequest {
        task: by_id::<ETask, _>(&ctx.db.worker_db, task_id)
            .await
            .context("load task")?
            .ok_or(ActionError::TaskNotFound { id: task_id })?,
        integration: integration_id,
        start_commit: evaluation_commit_of_job(&ctx.db.worker_db, job).await?,
        patch,
        title: render(title_template, DEFAULT_TITLE),
        body: render(body_template, DEFAULT_BODY),
        branch,
        base: None,
        job: Some(job),
    };

    match open_from_patch(ctx, request).await {
        Ok(opened) => opened_response(ctx, task_id, action_id, opened).await,
        Err(PullRequestError::Empty) => Ok(no_op()),
        Err(error) => Err(error.into()),
    }
}

fn job_of(payload: &JsonValue) -> Result<ExecJobId> {
    payload
        .get("job")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| anyhow!("exec_tree payload missing job"))?
        .parse()
        .map_err(|_| anyhow!("exec_tree payload names an invalid job"))
}

async fn tree_patch(ctx: &CiContext, job: ExecJobId) -> Result<Option<Vec<u8>>> {
    let written = outputs::one(&ctx.db.worker_db, job, TREE_PATCH_OUTPUT)
        .await
        .context("load the job's tree patch output")?
        .is_some_and(|output| output.size > 0);
    if !written {
        return Ok(None);
    }
    ctx.db
        .storage
        .nar_storage
        .get_exec_output(
            job.into_inner(),
            TREE_PATCH_OUTPUT,
            MAX_PULL_REQUEST_PATCH_BYTES,
        )
        .await
}

async fn opened_response(
    ctx: &CiContext,
    task_id: TaskId,
    action_id: TaskActionId,
    opened: OpenedPullRequest,
) -> Result<ExecutorOk> {
    upsert_open_pr_state(ctx, task_id, action_id, &opened.branch, opened.number, None).await?;
    Ok(ExecutorOk {
        status_code: Some(200),
        response_body: Some(
            serde_json::json!({ "pr_number": opened.number, "branch": opened.branch, "url": opened.url })
                .to_string(),
        ),
    })
}
