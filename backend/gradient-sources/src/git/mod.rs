/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod checkout;
mod commit_info;
mod context;
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
use tracing::instrument;

pub use checkout::{checkout_commit, checkout_commit_into};
pub use commit_info::HeadCommit;
pub use remote::BranchHead;
pub(crate) use ssh::with_private_key;

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
    let private_key = if gradient_types::input::check_repository_url_is_ssh(url) {
        let (private_key, _) = crate::ssh_key::decrypt_ssh_private_key(
            &ctx.config.secrets.crypt_file,
            project.clone(),
            &ctx.config.server.serve_url,
        )?;
        Some(private_key)
    } else {
        None
    };

    let url = url.to_owned();
    let branch = branch.map(|b| b.to_owned());

    let head = tokio::task::spawn_blocking(move || {
        remote::ls_remote_head(
            &url,
            private_key.as_ref().map(SecretString::expose),
            branch.as_deref(),
        )
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    })??;
    Ok(head.commit)
}

#[cfg(test)]
mod tests;
