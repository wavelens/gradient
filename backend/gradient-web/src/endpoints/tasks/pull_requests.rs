/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluations::COMMIT_HASH_BYTES;
use super::refusal::TaskRefusal;
use crate::api_spec::TaskPath;
use crate::authorization::Principal;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::refusal::{Refusal, RefusalStatus};
use crate::scope::{Requires, TaskScope, can};
use axum::extract::State;
use axum::{Extension, Json};
use gradient_ci::pull_requests::{
    PatchPullRequest, PullRequestError, default_branch_name, evaluation_commit_of_job,
    open_from_patch, patch_paths, pull_request_integration,
};
use gradient_core::ServerState;
use gradient_types::input::hex_to_vec;
use gradient_types::*;
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct CreatePullRequestRequest {
    pub title: String,
    pub body: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub patch: String,
    pub commit: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct PullRequestResponse {
    pub url: String,
    pub number: i64,
    pub branch: String,
}

#[derive(Debug, thiserror::Error)]
enum PullRequestRefusal {
    #[error("commit is required outside an exec job")]
    CommitRequired,
}

impl Refusal for PullRequestRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::BadRequest
    }
}

impl Refusal for PullRequestError {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::NoIntegration | Self::NoCommit => RefusalStatus::Conflict,
            Self::DeletesFile(_) | Self::DoesNotApply(_) | Self::Empty => {
                RefusalStatus::UnprocessableEntity
            }
            Self::Forge(_) => RefusalStatus::BadGateway,
        }
    }
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/pull-requests",
    operation_id = "openTaskPullRequest",
    tag = "tasks",
    summary = "Open a pull request from a patch",
    description = "Applies a unified diff to the start commit and opens or updates a pull request through the Git host integration of the task's `open_pr` action. Inside an exec job the start commit is the job's evaluation commit and the pull request is listed on the job; any other caller passes `commit`. Without `branch` the branch name follows the touched paths, so the same paths update one pull request. The patch may add and change files but not delete them.\n\n**Access:** Members with the `triggerEvaluation` permission, or the job token of an exec job of the task.",
    params(TaskPath),
    request_body = CreatePullRequestRequest,
    responses((status = OK, description = "Opened pull request", body = PullRequestResponse), WebError)
)]
pub async fn post_task_pull_request(
    State(state): State<Arc<ServerState>>,
    TaskScope { task, db, .. }: TaskScope<Requires<can::TriggerEvaluation>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<CreatePullRequestRequest>,
) -> WebResult<Json<BaseResponse<PullRequestResponse>>> {
    let job = principal.and_then(|Extension(principal)| principal.job().map(|job| job.job));
    let start_commit = start_commit(&db.web_db, job, body.commit.as_deref()).await?;
    let integration = pull_request_integration(&db.web_db, task.id)
        .await?
        .ok_or(PullRequestError::NoIntegration)?;
    let patch = body.patch.into_bytes();
    let branch = match body.branch {
        Some(branch) => branch,
        None => default_branch_name(job, &patch_paths(&patch)?),
    };

    let request = PatchPullRequest {
        task,
        integration,
        start_commit,
        patch,
        title: body.title,
        body: body.body.unwrap_or_default(),
        branch,
        base: body.base,
        job,
    };
    let opened = open_from_patch(&state.ci(), request).await?;
    Ok(ok_json(PullRequestResponse {
        url: opened.url,
        number: opened.number,
        branch: opened.branch,
    }))
}

async fn start_commit(
    db: &impl ConnectionTrait,
    job: Option<ExecJobId>,
    commit: Option<&str>,
) -> WebResult<String> {
    match job {
        Some(job) => Ok(evaluation_commit_of_job(db, job).await?),
        None => {
            let commit = commit.ok_or(PullRequestRefusal::CommitRequired)?;
            hex_to_vec(commit)
                .ok()
                .filter(|hash| hash.len() == COMMIT_HASH_BYTES)
                .ok_or(TaskRefusal::InvalidCommitHash)?;
            Ok(commit.to_ascii_lowercase())
        }
    }
}
