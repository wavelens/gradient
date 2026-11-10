/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::auto_attach::{integration_serves_repository, url_host};
use super::refusal::TaskRefusal;
use crate::api_spec::TaskPath;
use crate::endpoints::projects::integrations::WebhookBase;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::scope::{Requires, TaskScope, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_ci::IntegrationKind;
use gradient_ci::actions::decrypt_secret_with_file;
use gradient_ci::outbound_integration_access_token::refresh_and_get_usable_decrypted_access_token;
use gradient_core::ServerState;
use gradient_git_host::provision::{self, ForgeAccess, ProvisionError};
use gradient_sources::format_public_key;
use gradient_types::*;
use gradient_util::http::public_client;
use gradient_util::http_validation::validate_webhook_url;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct InstallWebhookRequest {
    pub inbound_integration: IntegrationId,
    pub outbound_integration: IntegrationId,
}

#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct InstallDeployKeyRequest {
    pub outbound_integration: IntegrationId,
}

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
    integrations: Vec<MIntegration>,
    repository: String,
    host: String,
}

impl Target {
    async fn load(scope: &TaskScope<Requires<can::ManageIntegrations>>) -> WebResult<Self> {
        let repository = provision::repository_path(&scope.task.repository)
            .ok_or_else(|| TaskRefusal::InvalidRepositoryUrl(scope.task.repository.clone()))?;
        let host = url_host(&scope.task.repository)
            .ok_or_else(|| TaskRefusal::InvalidRepositoryUrl(scope.task.repository.clone()))?;
        let integrations =
            gradient_db::integrations::of_project(&scope.db.web_db, scope.project.id).await?;
        Ok(Self {
            integrations,
            repository,
            host,
        })
    }

    fn integration(
        &self,
        repository_url: &str,
        id: IntegrationId,
        kind: IntegrationKind,
    ) -> WebResult<&MIntegration> {
        self.integrations
            .iter()
            .find(|i| {
                i.id == id
                    && i.kind == kind
                    && integration_serves_repository(repository_url, i, &self.integrations)
            })
            .ok_or_else(|| TaskRefusal::RepositoryIntegrationMismatch.into())
    }
}

fn decrypt(crypt_file: &str, encrypted: &str) -> WebResult<SecretString> {
    Ok(
        decrypt_secret_with_file(crypt_file, encrypted)
            .context("decrypt integration credential")?,
    )
}

fn forge_endpoint(outbound: &MIntegration, host: &str) -> WebResult<String> {
    let endpoint = outbound
        .endpoint_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| format!("https://{host}"));
    validate_webhook_url(&endpoint).map_err(|e| TaskRefusal::InvalidWebhookUrl(e.to_string()))?;
    Ok(endpoint)
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/repository/install-webhook",
    operation_id = "installTaskRepositoryWebhook",
    tag = "tasks",
    summary = "Install webhook on the Git host",
    description = "Create a webhook on the Git host. Send push, tag, pull request, comment, and release events to the selected webhook receiver. Sign them with the secret of that receiver.\n\nGradient can call the API of the Git host with the access token of the selected status reporter. Give that token admin rights on the repository.\n\nGitea, Forgejo, and GitLab work with this endpoint. GitHub repositories get events from a GitHub App instead.\n\nCallers need permission to manage integrations.\n\nReturn the repository path on the Git host. Return `502` with the reason after the Git host rejected the request.",
    params(TaskPath),
    request_body = InstallWebhookRequest,
    responses((status = OK, description = "Webhook installed", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_install_webhook(
    State(state): State<Arc<ServerState>>,
    scope: TaskScope<Requires<can::ManageIntegrations>>,
    JsonBody(body): JsonBody<InstallWebhookRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = Target::load(&scope).await?;
    let repository_url = &scope.task.repository;
    let inbound = target.integration(
        repository_url,
        body.inbound_integration,
        IntegrationKind::Inbound,
    )?;
    let outbound = target.integration(
        repository_url,
        body.outbound_integration,
        IntegrationKind::Outbound,
    )?;
    let endpoint_url = forge_endpoint(outbound, &target.host)?;

    let webhook_url = WebhookBase {
        serve_url: &state.config.server.serve_url,
        project: &scope.project.name,
    }
    .url_for(inbound)
    .ok_or(ProvisionError::Unsupported("GitHub"))?;
    let secret = decrypt(
        &state.config.secrets.crypt_file,
        inbound
            .secret
            .as_deref()
            .ok_or(TaskRefusal::ProvisionSecretMissing)?,
    )?;
    let token = refresh_and_get_usable_decrypted_access_token(
        &state.web_db,
        &state.config,
        &state.http,
        outbound,
    )
    .await
    .context("decrypt integration credential")?
    .ok_or(TaskRefusal::ProvisionTokenMissing)?;

    provision::install_webhook(
        public_client().clone(),
        &ForgeAccess {
            git_host: outbound.git_host_type,
            endpoint_url: &endpoint_url,
            token: token.expose(),
            repository: &target.repository,
        },
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
    description = "Add the project SSH key to the repository on the Git host as a read-only deploy key. Gradient clones that repository over SSH. Gradient can call the Git host API with the access token of the selected status reporter. Give that token admin permission on the repository. Supported Git hosts: Gitea, Forgejo, GitLab. Callers need permission to manage integrations. Return the repository path on the Git host. Return `502` and the reason for a rejection from the Git host.",
    params(TaskPath),
    request_body = InstallDeployKeyRequest,
    responses((status = OK, description = "Deploy key installed", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_install_deploy_key(
    State(state): State<Arc<ServerState>>,
    scope: TaskScope<Requires<can::ManageIntegrations>>,
    JsonBody(body): JsonBody<InstallDeployKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target = Target::load(&scope).await?;
    let outbound = target.integration(
        &scope.task.repository,
        body.outbound_integration,
        IntegrationKind::Outbound,
    )?;
    let endpoint_url = forge_endpoint(outbound, &target.host)?;
    let token = refresh_and_get_usable_decrypted_access_token(
        &state.web_db,
        &state.config,
        &state.http,
        outbound,
    )
    .await
    .context("decrypt integration credential")?
    .ok_or(TaskRefusal::ProvisionTokenMissing)?;
    let public_key = format_public_key(scope.project.clone(), &state.config.server.serve_url);
    let title = public_key
        .rsplit(' ')
        .next()
        .unwrap_or("gradient")
        .to_owned();

    provision::install_deploy_key(
        public_client().clone(),
        &ForgeAccess {
            git_host: outbound.git_host_type,
            endpoint_url: &endpoint_url,
            token: token.expose(),
            repository: &target.repository,
        },
        &title,
        &public_key,
    )
    .await?;
    Ok(ok_json(target.repository))
}
