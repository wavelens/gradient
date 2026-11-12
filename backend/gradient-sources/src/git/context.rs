/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ssh::SshGitPrivateKeyKnownHostsAndObservations;
use crate::SourceError;
use gradient_db::DbContext;
use gradient_db::project_ssh_host_keys::{
    apply_project_ssh_host_key_observations, list_project_ssh_host_keys_for_git_connections,
};
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait};

pub(super) struct TaskGitContext<'a> {
    pub(super) ctx: &'a DbContext,
    pub(super) task: &'a MTask,
    pub(super) ssh: Option<SshGitPrivateKeyKnownHostsAndObservations>,
}

impl<'a> TaskGitContext<'a> {
    pub(super) async fn new(ctx: &'a DbContext, task: &'a MTask) -> Result<Self, SourceError> {
        let ssh = if check_repository_url_is_ssh(&task.repository) {
            let project = EProject::find_by_id(task.project)
                .one(&ctx.worker_db)
                .await
                .map_err(database_error)?
                .ok_or(SourceError::ProjectNotFound { id: task.project })?;
            load_ssh_private_key_and_known_hosts(ctx, &project, &task.repository).await?
        } else {
            None
        };
        Ok(Self { ctx, task, ssh })
    }
}

pub async fn load_ssh_private_key_and_known_hosts(
    ctx: &DbContext,
    project: &MProject,
    url: &str,
) -> Result<Option<SshGitPrivateKeyKnownHostsAndObservations>, SourceError> {
    if !check_repository_url_is_ssh(url) {
        return Ok(None);
    }
    let (private_key, _) = crate::ssh_key::decrypt_ssh_private_key(
        &ctx.config.secrets.crypt_file,
        project.clone(),
        &ctx.config.server.serve_url,
    )?;
    let host_keys = list_project_ssh_host_keys_for_git_connections(&ctx.worker_db, project.id)
        .await
        .map_err(database_error)?;
    Ok(Some(SshGitPrivateKeyKnownHostsAndObservations::new(
        private_key,
        host_keys,
    )))
}

pub async fn save_observed_project_ssh_host_keys<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    ssh: Option<&SshGitPrivateKeyKnownHostsAndObservations>,
) {
    let Some(observations) = ssh.map(SshGitPrivateKeyKnownHostsAndObservations::observations)
    else {
        return;
    };
    if observations.is_empty() {
        return;
    }
    if let Err(error) = apply_project_ssh_host_key_observations(db, project, &observations).await {
        tracing::warn!(%error, %project, "Failed to save the observed host keys for SSH.");
    }
}

fn database_error(e: sea_orm::DbErr) -> SourceError {
    SourceError::Database {
        reason: e.to_string(),
    }
}
