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
    pub(super) ssh_creds: Option<(String, String)>,
}

impl<'a> TaskGitContext<'a> {
    pub(super) async fn new(ctx: &'a DbContext, task: &'a MTask) -> Result<Self, SourceError> {
        let url = &task.repository;
        let ssh_creds = if check_repository_url_is_ssh(url) {
            let project = EProject::find_by_id(task.project)
                .one(&ctx.worker_db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?
                .ok_or(SourceError::ProjectNotFound { id: task.project })?;
            Some(crate::ssh_key::decrypt_ssh_private_key(
                &ctx.config.secrets.crypt_file,
                project,
                &ctx.config.server.serve_url,
            )?)
        } else {
            None
        };
        Ok(Self {
            ctx,
            task,
            ssh_creds,
        })
    }
}
