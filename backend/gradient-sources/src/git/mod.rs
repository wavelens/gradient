/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Git source operations: remote ref polling ([`check_task_updates`]),
//! commit metadata ([`get_commit_info`]), HEAD resolution ([`resolve_head`]),
//! and SSH flake prefetch ([`Libgit2Prefetcher`]). The shared per-cycle state
//! lives in [`context::TaskGitContext`]; the public entry points below are
//! thin wrappers around it.

mod commit_info;
mod context;
mod pktline;
mod prefetch;
mod remote;
mod update_check;
mod url;

use crate::SourceError;
use context::TaskGitContext;
use gradient_db::DbContext;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use tracing::instrument;

pub use prefetch::Libgit2Prefetcher;
pub use remote::{accept_cert, fetch_options_with_ssh};

#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name))]
pub async fn check_task_updates(
    ctx: &DbContext,
    task: &MTask,
    branch: Option<&str>,
) -> Result<(bool, Vec<u8>), SourceError> {
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
) -> Result<(String, Option<String>, String), SourceError> {
    TaskGitContext::new(ctx, task)
        .await?
        .commit_info(commit_hash)
        .await
}

/// Best-effort: resolve the task's current HEAD (or branch) commit, message,
/// and author name. Used for manual trigger fires where we want a concrete
/// commit even if the polling source says "no update".
#[instrument(skip(ctx), fields(task_id = %task.id, task_name = %task.name))]
pub async fn resolve_head(
    ctx: &DbContext,
    task: &MTask,
    branch: Option<&str>,
) -> Result<(Vec<u8>, String, String), SourceError> {
    let (_has_update, commit_hash) = check_task_updates(ctx, task, branch).await?;
    let (msg, _email, author) = get_commit_info(ctx, task, &commit_hash).await?;
    Ok((commit_hash, msg, author))
}

/// Resolve a ref on an arbitrary remote repository to its commit hash, without
/// a task to hang it off. `branch = None` resolves the remote HEAD (the default
/// branch). SSH URLs are authenticated with `project`'s deploy key, the same key
/// the worker later uses to fetch the flake.
#[instrument(skip(ctx, project), fields(project_id = %project.id, repository = %url))]
pub async fn resolve_remote_ref(
    ctx: &DbContext,
    project: &MProject,
    url: &str,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let ssh_creds = if gradient_types::input::check_repository_url_is_ssh(url) {
        Some(crate::ssh_key::decrypt_ssh_private_key(
            &ctx.config.secrets.crypt_secret_file,
            project.clone(),
            &ctx.config.server.serve_url,
        )?)
    } else {
        None
    };

    let url = url.to_owned();
    let branch = branch.map(|b| b.to_owned());

    tokio::task::spawn_blocking(move || match ssh_creds {
        Some((private_key, public_key)) => remote::ls_remote_head(
            &url,
            Some(&private_key),
            Some(&public_key),
            branch.as_deref(),
        ),
        None => remote::ls_remote_head(&url, None, None, branch.as_deref()),
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    })?
}

#[cfg(test)]
mod tests;
