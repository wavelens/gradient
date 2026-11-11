/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ActionError;
use crate::context::CiContext;
use crate::task_commit_status_posting::build_commit_status_posting_client_from_git_host_connection_id;
use anyhow::{Context, Result};
use gradient_types::ActionConfig;

/// The regular synthetic test-fire is posting against placeholder owner, repo and sha, which Git
/// hosts reject. A connectivity check is the only test these actions can pass.
pub async fn verify_git_host_action(
    ctx: &CiContext,
    action: &gradient_types::MTaskAction,
    repository: &str,
) -> Result<()> {
    let ActionConfig::OpenPr {
        git_host_connection_id,
        ..
    } = serde_json::from_value::<ActionConfig>(action.config.clone())
        .context("decode action config")?
    else {
        anyhow::bail!("action does not use a Git host connection");
    };

    let (owner, repo) =
        crate::parse_owner_repo(repository).ok_or_else(|| ActionError::RepositoryUnparsable {
            repository: repository.to_owned(),
        })?;
    let reporter =
        build_commit_status_posting_client_from_git_host_connection_id(ctx, git_host_connection_id)
            .await?;

    reporter
        .verify(&owner, &repo)
        .await
        .context("check Git host connectivity")
}
