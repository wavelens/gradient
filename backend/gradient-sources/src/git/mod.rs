/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod checkout;
mod commit_info;
mod context;
mod git_credentials;
mod git_network_limits;
mod https_credentials_for_operation_host;
mod pktline;
mod remote;
mod ssh;
mod update_check;
mod url;

use crate::SourceError;
use context::TaskGitContext;
use gradient_db::DbContext;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use sea_orm::ConnectionTrait;
use tracing::instrument;

pub use checkout::{checkout_commit, checkout_commit_into};
pub use commit_info::HeadCommit;
pub use context::{
    load_project_git_credentials_for_repository_url, repository_url_may_need_stored_credentials,
    save_observed_project_ssh_host_keys,
};
pub use git_credentials::{GitOperationCredentials, HttpsGitCredential};
pub use remote::BranchHead;
pub use ssh::{SshGitPrivateKeyKnownHostsAndObservations, fetch_ssh_host_key};

#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name))]
pub async fn check_task_updates(
    ctx: &DbContext,
    task: &MTask,
    branch: Option<&str>,
) -> Result<(bool, BranchHead), SourceError> {
    TaskGitContext::new(ctx, task)
        .await?
        .check_for_updates(branch)
        .await
}

#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name))]
pub async fn remote_branch_head(
    ctx: &DbContext,
    task: &MTask,
    branch: Option<&str>,
) -> Result<BranchHead, SourceError> {
    TaskGitContext::new(ctx, task)
        .await?
        .remote_head(branch)
        .await
}

#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name))]
pub async fn remote_tag_commit(
    ctx: &DbContext,
    task: &MTask,
    tag: &str,
) -> Result<Vec<u8>, SourceError> {
    TaskGitContext::new(ctx, task)
        .await?
        .remote_tag_commit(tag)
        .await
}

#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name, commit_hash = %vec_to_hex(commit_hash)))]
pub async fn get_commit_info(
    ctx: &DbContext,
    task: &MTask,
    commit_hash: &[u8],
) -> Result<HeadCommit, SourceError> {
    TaskGitContext::new(ctx, task)
        .await?
        .commit_info(commit_hash)
        .await
}

#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name))]
pub async fn head_commit(
    ctx: &DbContext,
    task: &MTask,
    branch: Option<&str>,
) -> Result<(String, HeadCommit), SourceError> {
    TaskGitContext::new(ctx, task)
        .await?
        .head_commit(branch)
        .await
}

#[instrument(skip(ctx, project), fields(project_id = %project.id, repository = %url))]
pub async fn resolve_remote_ref(
    ctx: &DbContext,
    project: &MProject,
    url: &str,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let credentials = load_project_git_credentials_for_repository_url(ctx, project, url).await?;
    list_remote_refs_find_branch_head_and_save_host_keys(
        &ctx.worker_db,
        project.id,
        url,
        credentials,
        branch,
    )
    .await
}

async fn list_remote_refs_find_branch_head_and_save_host_keys<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    url: &str,
    credentials: GitOperationCredentials,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let (url, branch, blocking_credentials) = (
        url.to_owned(),
        branch.map(str::to_owned),
        credentials.clone(),
    );
    let head = tokio::task::spawn_blocking(move || {
        remote::ls_remote_head(&url, &blocking_credentials, branch.as_deref())
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    });
    save_observed_project_ssh_host_keys(
        db,
        project,
        credentials.ssh_git_private_key_and_known_hosts.as_ref(),
    )
    .await;
    Ok(head??.commit)
}

pub(crate) fn remote_ref_commit(
    url: &str,
    ref_: Option<&str>,
    credentials: &GitOperationCredentials,
) -> Result<String, SourceError> {
    let refs = remote::list_remote_refs(url, credentials)?;
    remote::ref_commit(&refs, ref_).map(|commit| vec_to_hex(&commit))
}

#[cfg(test)]
mod tests;
