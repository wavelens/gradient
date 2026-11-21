/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ActionError;
use crate::context::CiContext;
use crate::task_commit_status_posting::build_task_repository_reporter_if_git_host_connection_allowed;
use anyhow::{Context, Result};
use gradient_types::{ActionConfig, MTask, MTaskAction};

/// The regular synthetic test-fire is posting against placeholder owner, repo and sha, which Git
/// hosts reject. A connectivity check is the only test these actions can pass.
pub async fn verify_git_host_action(
    ctx: &CiContext,
    action: &MTaskAction,
    task: &MTask,
) -> Result<()> {
    let ActionConfig::OpenPr {
        git_host_connection_id,
        ..
    } = serde_json::from_value::<ActionConfig>(action.config.clone())
        .context("decode action config")?
    else {
        anyhow::bail!("action does not use a Git host connection");
    };

    let (owner, repo) = crate::parse_owner_repo(&task.repository).ok_or_else(|| {
        ActionError::RepositoryUnparsable {
            repository: task.repository.clone(),
        }
    })?;
    let reporter = build_task_repository_reporter_if_git_host_connection_allowed(
        ctx,
        task,
        git_host_connection_id,
    )
    .await??;

    reporter
        .verify(&owner, &repo)
        .await
        .context("check Git host connectivity")
}
