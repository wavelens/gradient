/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Git source operations: remote ref polling ([`check_project_updates`]),
//! commit metadata ([`get_commit_info`]), HEAD resolution ([`resolve_head`]),
//! and SSH flake prefetch ([`Libgit2Prefetcher`]). The shared per-cycle state
//! lives in [`context::ProjectGitContext`]; the public entry points below are
//! thin wrappers around it.

mod commit_info;
mod context;
mod pktline;
mod prefetch;
mod remote;
mod update_check;
mod url;

use crate::db::DbContext;
use crate::sources::SourceError;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use context::ProjectGitContext;
use tracing::instrument;

pub use prefetch::Libgit2Prefetcher;
pub use remote::accept_cert;

#[instrument(skip(ctx), fields(project_id = %project.id, project_name = %project.name))]
pub async fn check_project_updates(
    ctx: &DbContext,
    project: &MProject,
    branch: Option<&str>,
) -> Result<(bool, Vec<u8>), SourceError> {
    ProjectGitContext::new(ctx, project)
        .await?
        .check_for_updates(branch)
        .await
}

#[instrument(skip(ctx), fields(project_id = %project.id, project_name = %project.name, commit_hash = %vec_to_hex(commit_hash)))]
pub async fn get_commit_info(
    ctx: &DbContext,
    project: &MProject,
    commit_hash: &[u8],
) -> Result<(String, Option<String>, String), SourceError> {
    ProjectGitContext::new(ctx, project)
        .await?
        .commit_info(commit_hash)
        .await
}

/// Best-effort: resolve the project's current HEAD (or branch) commit, message,
/// and author name. Used for manual trigger fires where we want a concrete
/// commit even if the polling source says "no update".
#[instrument(skip(ctx), fields(project_id = %project.id, project_name = %project.name))]
pub async fn resolve_head(
    ctx: &DbContext,
    project: &MProject,
    branch: Option<&str>,
) -> Result<(Vec<u8>, String, String), SourceError> {
    let (_has_update, commit_hash) = check_project_updates(ctx, project, branch).await?;
    let (msg, _email, author) = get_commit_info(ctx, project, &commit_hash).await?;
    Ok((commit_hash, msg, author))
}

#[cfg(test)]
mod tests;
