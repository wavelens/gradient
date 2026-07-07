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

/// Bundles the server state, project reference, and (if the repository URL is
/// SSH) the decrypted key pair for the project's owning organisation.
///
/// Created once per project-check cycle via [`ProjectGitContext::new`]. Both
/// [`check_project_updates`](super::check_project_updates) and
/// [`get_commit_info`](super::get_commit_info) are thin wrappers that construct
/// this context and call the corresponding method, so the DB round-trip and key
/// decryption only happen once even when both are called in sequence (e.g. in
/// `dispatch::poll_projects_for_evaluations`).
pub(super) struct ProjectGitContext<'a> {
    pub(super) ctx: &'a DbContext,
    pub(super) project: &'a MProject,
    /// `Some((private_key, public_key))` for SSH repos; `None` for HTTPS/git.
    pub(super) ssh_creds: Option<(String, String)>,
}

impl<'a> ProjectGitContext<'a> {
    /// Resolve SSH credentials from the DB if the repository URL is SSH.
    pub(super) async fn new(
        ctx: &'a DbContext,
        project: &'a MProject,
    ) -> Result<Self, SourceError> {
        let url = &project.repository;
        let ssh_creds = if check_repository_url_is_ssh(url) {
            let organization = EOrganization::find_by_id(project.organization)
                .one(&ctx.worker_db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?
                .ok_or(SourceError::OrganizationNotFound {
                    id: project.organization,
                })?;
            Some(crate::ssh_key::decrypt_ssh_private_key(
                &ctx.config.secrets.crypt_secret_file,
                organization,
                &ctx.config.server.serve_url,
            )?)
        } else {
            None
        };
        Ok(Self {
            ctx,
            project,
            ssh_creds,
        })
    }
}
