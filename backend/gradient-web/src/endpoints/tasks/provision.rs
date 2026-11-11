/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TaskRefusal;
use super::repository::check_git_host_connection_fits_task;
use crate::api_spec::TaskPath;
use crate::endpoints::git_host_connections::{url_host, webhook_url};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{Requires, TaskScope, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_ci::CiContext;
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_ci::git_host_connection_token_refresh::refresh_and_get_usable_decrypted_access_token;
use gradient_core::ServerState;
use gradient_git_host::provision::{self, ForgeAccess, ProvisionError};
use gradient_sources::format_public_key;
use gradient_types::*;
use gradient_util::http::public_client;
use gradient_util::http_validation::validate_webhook_url;
use std::sync::Arc;

impl From<ProvisionError> for WebError {
    fn from(e: ProvisionError) -> Self {
        match e {
            ProvisionError::Unsupported(_) => TaskRefusal::ProvisionUnsupported(e.to_string()),
            ProvisionError::Host(reason) => TaskRefusal::ProvisionRefused { reason },
        }
        .into()
    }
}

struct Target {
    connection: MGitHostConnection,
    repository: String,
    base_url: String,
}

impl Target {
    async fn load(scope: &TaskScope<Requires<can::ManageIntegrations>>) -> WebResult<Self> {
        let task_repository = &scope.task.repository;
        let repository = provision::repository_path(task_repository)
            .ok_or_else(|| TaskRefusal::InvalidRepositoryUrl(task_repository.clone()))?;
        let host = url_host(task_repository)
            .ok_or_else(|| TaskRefusal::InvalidRepositoryUrl(task_repository.clone()))?;
        let id = scope
            .task
            .git_host_connection
            .ok_or(TaskRefusal::GitHostConnectionMissing)?;
        let connection = check_git_host_connection_fits_task(
            &scope.db.web_db,
            &scope.project,
            task_repository,
            id,
        )
        .await?;
        if connection.github_installation_id.is_some() {
            return Err(ProvisionError::Unsupported("GitHub").into());
        }
        let base_url = connection
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| format!("https://{host}"));
        validate_webhook_url(&base_url)
            .map_err(|e| TaskRefusal::InvalidWebhookUrl(e.to_string()))?;
        Ok(Self {
            connection,
            repository,
            base_url,
        })
    }

    async fn token(&self, ci: &CiContext) -> WebResult<SecretString> {
        refresh_and_get_usable_decrypted_access_token(
            ci.db.web_db.inner(),
            &ci.db.config,
            &ci.http,
            &self.connection,
        )
        .await?
        .ok_or_else(|| TaskRefusal::ProvisionTokenMissing.into())
    }

    fn access<'a>(&'a self, token: &'a SecretString) -> ForgeAccess<'a> {
        ForgeAccess {
            git_host: self.connection.git_host_type,
            endpoint_url: &self.base_url,
            token: token.expose(),
            repository: &self.repository,
        }
    }
}

fn decrypt(crypt_file: &str, encrypted: &str) -> WebResult<SecretString> {
    Ok(decrypt_secret_with_file(crypt_file, encrypted)
        .context("decrypt git host connection credential")?)
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/repository/install-webhook",
    operation_id = "installTaskRepositoryWebhook",
    tag = "tasks",
    summary = "Install webhook on the Git host",
    description = "Create a webhook on the Git host. The webhook can deliver push, tag, pull request, comment and release events to the task's connection to the Git host. Use the secret of that connection to sign each request.\n\nGradient can call the API of the Git host with the access token of the connection. Grant admin permission for the repository to that token.\n\nGitea, Forgejo and GitLab support the endpoint. GitHub repositories receive events from a GitHub App instead.\n\nThe caller needs permission to manage integrations.\n\nReturn the repository path on the Git host. The API can return `502` with the reason after a rejection from the Git host.",
    params(TaskPath),
    responses((status = OK, description = "Webhook installed", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_install_webhook(
    State(state): State<Arc<ServerState>>,
    scope: TaskScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = Target::load(&scope).await?;
    let webhook_url = webhook_url(&state.config.server.serve_url, &target.connection)
        .ok_or(TaskRefusal::ProvisionSecretMissing)?;
    let secret = decrypt(
        &state.config.secrets.crypt_file,
        target
            .connection
            .secret
            .as_deref()
            .ok_or(TaskRefusal::ProvisionSecretMissing)?,
    )?;
    let token = target.token(&state.ci()).await?;

    provision::install_webhook(
        public_client().clone(),
        &target.access(&token),
        &webhook_url,
        secret.expose(),
    )
    .await?;
    Ok(ok_json(target.repository))
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/repository/install-deploy-key",
    operation_id = "installTaskRepositoryDeployKey",
    tag = "tasks",
    summary = "Install deploy key on the Git host",
    description = "Add the SSH key of the project as a read-only deploy key to a repository on the Git host. Gradient can then clone over SSH. Gradient can call the API of the Git host with the access token for the task's connection to the Git host. Grant admin permissions for the repository to that token. Supported Git hosts are Gitea, Forgejo, and GitLab. The caller must have permission to manage integrations. Return the repository path on the Git host. Return `502` and the reason after a rejection from the Git host.",
    params(TaskPath),
    responses((status = OK, description = "Deploy key installed", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_install_deploy_key(
    State(state): State<Arc<ServerState>>,
    scope: TaskScope<Requires<can::ManageIntegrations>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = Target::load(&scope).await?;
    let token = target.token(&state.ci()).await?;
    let public_key = format_public_key(scope.project.clone(), &state.config.server.serve_url);
    let title = public_key
        .rsplit(' ')
        .next()
        .unwrap_or("gradient")
        .to_owned();

    provision::install_deploy_key(
        public_client().clone(),
        &target.access(&token),
        &title,
        &public_key,
    )
    .await?;
    Ok(ok_json(target.repository))
}
