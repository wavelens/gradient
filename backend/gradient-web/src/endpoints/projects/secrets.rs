/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::ProjectRefusal;
use crate::api_spec::{ProjectPath, ProjectSecretPath};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::resource::Resource;
use crate::scope::{Member, PathParams, ProjectScope, Requires, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_core::ServerState;
use gradient_types::*;
use gradient_wire::types::validate_secret_name;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

const MAX_SECRET_VALUE_BYTES: usize = 65536;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ProjectSecretSummary {
    pub name: String,
    pub updated_at: chrono::NaiveDateTime,
    pub updated_by: Option<String>,
    pub managed: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PutProjectSecretRequest {
    pub value: String,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct SopsRecipientResponse {
    pub age: Option<String>,
    pub ssh: String,
}

fn summary(secret: MProjectSecret, usernames: &BTreeMap<UserId, String>) -> ProjectSecretSummary {
    ProjectSecretSummary {
        updated_by: secret
            .updated_by
            .and_then(|user| usernames.get(&user).cloned()),
        name: secret.name,
        updated_at: secret.updated_at,
        managed: secret.managed,
    }
}

async fn usernames(
    db: &impl sea_orm::ConnectionTrait,
    secrets: &[MProjectSecret],
) -> Result<BTreeMap<UserId, String>, WebError> {
    let mut ids: Vec<UserId> = secrets.iter().filter_map(|s| s.updated_by).collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }

    Ok(gradient_db::users::with_ids(db, &ids)
        .await?
        .into_iter()
        .map(|user| (user.id, user.username))
        .collect())
}

fn secret_name(params: &PathParams) -> WebResult<String> {
    let name: String = params.param("name")?;
    validate_secret_name(&name).map_err(|e| ProjectRefusal::InvalidSecretName {
        reason: e.to_string(),
    })?;
    Ok(name)
}

fn check_value(value: &str) -> WebResult<()> {
    if value.is_empty() {
        return Err(ProjectRefusal::EmptySecretValue.into());
    }
    if value.len() > MAX_SECRET_VALUE_BYTES {
        return Err(ProjectRefusal::SecretValueTooLarge.into());
    }

    Ok(())
}

fn refuse_managed(secret: &MProjectSecret) -> WebResult<()> {
    if secret.managed {
        return Err(ProjectRefusal::ManagedSecret {
            name: secret.name.clone(),
        }
        .into());
    }

    Ok(())
}

#[utoipa::path(
    get,
    path = "/projects/{project}/secrets",
    operation_id = "listProjectSecrets",
    tag = "projects",
    summary = "List project secrets",
    description = "Lists the secrets exec jobs of this project can request. Values are write-only and never returned.",
    params(ProjectPath),
    responses((status = OK, description = "Secrets by name", body = Vec<ProjectSecretSummary>), WebError)
)]
pub async fn list_project_secrets(
    ProjectScope { project, db, .. }: ProjectScope<Requires<can::ManageProjectSettings>>,
) -> WebResult<Json<BaseResponse<Vec<ProjectSecretSummary>>>> {
    let secrets = gradient_db::project_secrets::list(&db.web_db, project.id).await?;
    let names = usernames(&db.web_db, &secrets).await?;

    Ok(ok_json(
        secrets.into_iter().map(|s| summary(s, &names)).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/secrets/{name}",
    operation_id = "getProjectSecret",
    tag = "projects",
    summary = "Get a project secret",
    description = "Returns when and by whom a secret was last set, never its value.",
    params(ProjectSecretPath),
    responses((status = OK, description = "Secret", body = ProjectSecretSummary), WebError)
)]
pub async fn get_project_secret(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageProjectSettings>>,
) -> WebResult<Json<BaseResponse<ProjectSecretSummary>>> {
    let name = secret_name(&params)?;
    let secret = gradient_db::project_secrets::one(&db.web_db, project.id, &name)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::ProjectSecret))?;
    let names = usernames(&db.web_db, std::slice::from_ref(&secret)).await?;

    Ok(ok_json(summary(secret, &names)))
}

#[utoipa::path(
    put,
    path = "/projects/{project}/secrets/{name}",
    operation_id = "putProjectSecret",
    tag = "projects",
    summary = "Set a project secret",
    description = "Creates or replaces a secret. The value is stored encrypted with the server's crypt key. Secrets set by the declarative state cannot be replaced.",
    params(ProjectSecretPath),
    request_body = PutProjectSecretRequest,
    responses((status = OK, description = "Secret stored", body = ProjectSecretSummary), WebError)
)]
pub async fn put_project_secret(
    ProjectScope {
        project,
        user,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageProjectSettings>>,
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<PutProjectSecretRequest>,
) -> WebResult<Json<BaseResponse<ProjectSecretSummary>>> {
    let name = secret_name(&params)?;
    check_value(&body.value)?;
    if let Some(existing) = gradient_db::project_secrets::one(&db.web_db, project.id, &name).await?
    {
        refuse_managed(&existing)?;
    }

    let encrypted = encrypt_secret_with_file(&state.config.secrets.crypt_file, &body.value)
        .context("encrypt project secret")?;
    let secret = gradient_db::project_secrets::upsert(
        &db.web_db,
        project.id,
        &name,
        encrypted,
        false,
        Some(user.id),
    )
    .await?;

    Ok(ok_json(summary(
        secret,
        &BTreeMap::from([(user.id, user.username)]),
    )))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/secrets/{name}",
    operation_id = "deleteProjectSecret",
    tag = "projects",
    summary = "Delete a project secret",
    description = "Deletes a secret. Secrets set by the declarative state cannot be deleted here.",
    params(ProjectSecretPath),
    responses((status = OK, description = "Secret deleted", body = bool), WebError)
)]
pub async fn delete_project_secret(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<Requires<can::ManageProjectSettings>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = secret_name(&params)?;
    let secret = gradient_db::project_secrets::one(&db.web_db, project.id, &name)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::ProjectSecret))?;
    refuse_managed(&secret)?;
    gradient_db::project_secrets::delete(&db.web_db, project.id, &name).await?;

    Ok(ok_json(true))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/sops-recipient",
    operation_id = "getProjectSopsRecipient",
    tag = "projects",
    summary = "Get the sops recipient",
    description = "Returns the recipient to encrypt repository sops files for, derived from the project SSH key. `age` is absent for an RSA key, use `ssh` then.",
    params(ProjectPath),
    responses((status = OK, description = "Sops recipient", body = SopsRecipientResponse), WebError)
)]
pub async fn get_project_sops_recipient(
    ProjectScope { project, .. }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<SopsRecipientResponse>>> {
    let recipient = gradient_sops::sops_recipient(&project.public_key).map_err(|e| {
        ProjectRefusal::UnusableSopsKey {
            reason: e.to_string(),
        }
    })?;

    Ok(ok_json(SopsRecipientResponse {
        age: recipient.age,
        ssh: recipient.ssh,
    }))
}
