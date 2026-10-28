/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::SourceError;
use gradient_db::DbContext;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use sea_orm::EntityTrait;

pub(super) struct TaskGitContext<'a> {
    pub(super) ctx: &'a DbContext,
    pub(super) task: &'a MTask,
    pub(super) private_key: Option<SecretString>,
}

impl<'a> TaskGitContext<'a> {
    pub(super) async fn new(ctx: &'a DbContext, task: &'a MTask) -> Result<Self, SourceError> {
        let url = &task.repository;
        let private_key = if check_repository_url_is_ssh(url) {
            let project = EProject::find_by_id(task.project)
                .one(&ctx.worker_db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?
                .ok_or(SourceError::ProjectNotFound { id: task.project })?;
            let (private_key, _) = crate::ssh_key::decrypt_ssh_private_key(
                &ctx.config.secrets.crypt_file,
                project,
                &ctx.config.server.serve_url,
            )?;
            Some(private_key)
        } else {
            None
        };
        Ok(Self {
            ctx,
            task,
            private_key,
        })
    }
}
