/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_credentials::{GitOperationCredentials, HttpsGitCredential};
use super::ssh::SshGitPrivateKeyKnownHostsAndObservations;
use crate::SourceError;
use gradient_db::DbContext;
use gradient_db::project_git_https_credentials::one_for_project_and_host;
use gradient_db::project_ssh_host_keys::{
    apply_project_ssh_host_key_observations, list_project_ssh_host_keys_for_git_connections,
};
use gradient_types::https_git_url_host::https_host_from_repository_url;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait};

pub(super) struct TaskGitContext<'a> {
    pub(super) ctx: &'a DbContext,
    pub(super) task: &'a MTask,
    pub(super) credentials: GitOperationCredentials,
}

impl<'a> TaskGitContext<'a> {
    pub(super) async fn new(ctx: &'a DbContext, task: &'a MTask) -> Result<Self, SourceError> {
        let credentials = if repository_url_may_need_stored_credentials(&task.repository) {
            let project = EProject::find_by_id(task.project)
                .one(&ctx.worker_db)
                .await
                .map_err(database_error)?
                .ok_or(SourceError::ProjectNotFound { id: task.project })?;
            load_project_git_credentials_for_repository_url(ctx, &project, &task.repository).await?
        } else {
            GitOperationCredentials::default()
        };
        Ok(Self {
            ctx,
            task,
            credentials,
        })
    }
}

pub fn repository_url_may_need_stored_credentials(url: &str) -> bool {
    check_repository_url_is_ssh(url) || https_host_from_repository_url(url).is_some()
}

pub async fn load_project_git_credentials_for_repository_url(
    ctx: &DbContext,
    project: &MProject,
    url: &str,
) -> Result<GitOperationCredentials, SourceError> {
    Ok(GitOperationCredentials {
        ssh_git_private_key_and_known_hosts: load_ssh_private_key_and_known_hosts(
            ctx, project, url,
        )
        .await?,
        https_git_credentials: load_decrypted_project_https_credentials_for_repository_url_host(
            ctx, project, url,
        )
        .await?,
    })
}

async fn load_ssh_private_key_and_known_hosts(
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

async fn load_decrypted_project_https_credentials_for_repository_url_host(
    ctx: &DbContext,
    project: &MProject,
    url: &str,
) -> Result<Vec<HttpsGitCredential>, SourceError> {
    let Some(host) = https_host_from_repository_url(url) else {
        return Ok(Vec::new());
    };
    let Some(stored) = one_for_project_and_host(&ctx.worker_db, project.id, &host)
        .await
        .map_err(database_error)?
    else {
        return Ok(Vec::new());
    };
    let password = crate::secret::decrypt_secret(&ctx.config.secrets.crypt_file, &stored.password)?;
    Ok(vec![HttpsGitCredential {
        host: stored.host,
        username: stored.username,
        password,
    }])
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
