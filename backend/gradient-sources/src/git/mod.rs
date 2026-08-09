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

#[cfg(test)]
mod tests;
