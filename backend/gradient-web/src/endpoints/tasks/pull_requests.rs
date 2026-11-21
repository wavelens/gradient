/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluations::COMMIT_HASH_BYTES;
use super::refusal::TaskRefusal;
use crate::api_spec::TaskPath;
use crate::audit::Audit;
use crate::authorization::Principal;
use crate::error::{WebError, WebResult};
use crate::extractors::JsonBody;
use crate::helpers::ok_json;
use crate::refusal::{Refusal, RefusalStatus};
use crate::scope::{Requires, TaskScope, can};
use axum::extract::State;
use axum::{Extension, Json};
use gradient_ci::pull_requests::{
    PatchPullRequest, PullRequestError, default_branch_name, evaluation_commit_of_job,
    open_from_patch, patch_paths, pull_request_git_host_connection,
};
use gradient_core::ServerState;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
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
    #[error("Values of `branch` contain only letters, digits and `. _ - /`.")]
    BranchNameContainsInvalidCharacters,
    #[error(
        "Values of `base` follow the git rule for branch names: no `..`, no control characters, no spaces and no `~ ^ : ? * [ \\`."
    )]
    BaseBranchNameContainsInvalidCharacters,
}

impl Refusal for PullRequestRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::BadRequest
    }
}

impl Refusal for PullRequestError {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::NoGitHostConnection | Self::NoCommit | Self::InvalidRepositoryUrl(_) => {
                RefusalStatus::Conflict
            }
            Self::GitHostConnectionCannotActOnTaskRepository(_) => RefusalStatus::Forbidden,
            Self::DeletesFile(_)
            | Self::DoesNotApply(_)
            | Self::PathIsNotRelativeInsideRepository(_)
            | Self::PathIsNotRegularFile { .. }
            | Self::PatchExceedsMaximumFileCount { .. }
            | Self::PatchExceedsSizeLimit { .. }
            | Self::Empty => RefusalStatus::UnprocessableEntity,
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
    description = "Apply a unified diff to the start commit and create or update a pull request through the Git host connection of the task's `open_pr` action. Exec jobs use the job's evaluation commit as the start commit and can show the pull request on the job. Other callers pass `commit`. Segments in the specified `branch` use `/` as separator. All segments stay non-empty. All segments contain only letters, digits, `.`, `_` and `-`. Segments never start with `.`, never contain `..` and never end with `.lock`. Names for omitted `branch` live under `gradient/`. Git rules for branch names apply to the specified `base`. Callers that omit `branch` get a branch name from the changed paths, so the same path can update one pull request. A patch can add and change files but cannot delete files.\n\n**Access:** Members with the `triggerEvaluation` permission, or the job token of an exec job of the task.",
    params(TaskPath),
    request_body = CreatePullRequestRequest,
    responses((status = OK, description = "Opened pull request", body = PullRequestResponse), WebError)
)]
pub async fn post_task_pull_request(
    State(state): State<Arc<ServerState>>,
    TaskScope { task, db, .. }: TaskScope<Requires<can::TriggerEvaluation>>,
    principal: Option<Extension<Principal>>,
    audit: Audit,
    JsonBody(body): JsonBody<CreatePullRequestRequest>,
) -> WebResult<Json<BaseResponse<PullRequestResponse>>> {
    let job = principal.and_then(|Extension(principal)| principal.job().map(|job| job.job));
    if let Some(branch) = &body.branch {
        check_branch_name_has_safe_ref_characters(branch)?;
    }
    if let Some(base) = &body.base {
        check_base_branch_name_follows_git_branch_name_rule(base)?;
    }
    let start_commit = start_commit(&db.web_db, job, body.commit.as_deref()).await?;
    let git_host_connection = pull_request_git_host_connection(&db.web_db, task.id)
        .await?
        .ok_or(PullRequestError::NoGitHostConnection)?;
    let patch = body.patch.into_bytes();
    let branch = match body.branch {
        Some(branch) => branch,
        None => default_branch_name(job, &patch_paths(&patch)?),
    };

    let owner = EventOwner::task(task.project, task.id);
    let request = PatchPullRequest {
        task,
        git_host_connection,
        start_commit,
        patch,
        title: body.title,
        body: body.body.unwrap_or_default(),
        branch,
        base: body.base,
        job,
    };
    let opened = open_from_patch(&state.ci(), request).await?;
    let metadata = serde_json::json!({
        "branch": opened.branch,
        "number": opened.number,
        "url": opened.url,
        "job": job.map(|job| job.to_string()),
    });
    audit
        .write(Action::PullRequestCreateOrUpdateFromPatch, owner, metadata)
        .await;
    Ok(ok_json(PullRequestResponse {
        url: opened.url,
        number: opened.number,
        branch: opened.branch,
    }))
}

fn check_branch_name_has_safe_ref_characters(branch: &str) -> Result<(), PullRequestRefusal> {
    let valid = branch.split('/').all(|segment| {
        !segment.is_empty()
            && !segment.starts_with('.')
            && !segment.ends_with(".lock")
            && !segment.contains("..")
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    });
    if valid {
        Ok(())
    } else {
        Err(PullRequestRefusal::BranchNameContainsInvalidCharacters)
    }
}

fn check_base_branch_name_follows_git_branch_name_rule(
    base: &str,
) -> Result<(), PullRequestRefusal> {
    if gradient_sources::git::is_valid_branch_name(base) {
        Ok(())
    } else {
        Err(PullRequestRefusal::BaseBranchNameContainsInvalidCharacters)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_branch_name_has_safe_ref_characters_accepts_valid_branch_names() {
        for branch in [
            "gradient/fix",
            "gradient/a/b-c_d.e",
            "gradient/patch/0123abcd",
        ] {
            assert!(check_branch_name_has_safe_ref_characters(branch).is_ok());
        }
    }

    #[test]
    fn check_branch_name_has_safe_ref_characters_rejects_empty_name_empty_segment_parent_directory_leading_dot_lock_suffix_space_and_tilde()
     {
        for branch in ["", "/x", "x/", "../main", ".hidden", "x.lock", "a b", "a~1"] {
            assert!(
                check_branch_name_has_safe_ref_characters(branch).is_err(),
                "{branch}"
            );
        }
    }

    #[test]
    fn base_branch_check_accepts_valid_git_branch_names() {
        for base in ["main", "release/1.0", "feature+x", "user@fix", "fix#12"] {
            assert!(
                check_base_branch_name_follows_git_branch_name_rule(base).is_ok(),
                "{base}"
            );
        }
    }

    #[test]
    fn base_branch_check_rejects_names_git_refuses() {
        for base in ["../main", "a..b", "fix\nx", "fix\u{7}"] {
            assert!(
                matches!(
                    check_base_branch_name_follows_git_branch_name_rule(base),
                    Err(PullRequestRefusal::BaseBranchNameContainsInvalidCharacters)
                ),
                "{base:?}"
            );
        }
    }
}
