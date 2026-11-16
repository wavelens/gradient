/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::ProjectRefusal;
use crate::api_spec::{ProjectGitHttpsCredentialPathParameters, ProjectPath};
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::extractors::JsonBody;
use crate::helpers::ok_json;
use crate::resource::Resource;
use crate::scope::{Member, ProjectScope, RequiresUnmanaged, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_entity::ids::ProjectGitHttpsCredentialId;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::https_git_url_host::normalize_bare_https_git_host;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GitHttpsCredentialResponse {
    pub id: ProjectGitHttpsCredentialId,
    pub host: String,
    pub username: String,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SaveGitHttpsCredentialRequestBody {
    pub host: String,
    pub username: String,
    pub password: String,
}

fn project_git_https_credential_row_to_response(
    row: MProjectGitHttpsCredential,
) -> GitHttpsCredentialResponse {
    GitHttpsCredentialResponse {
        id: row.id,
        host: row.host,
        username: row.username,
        updated_at: row.updated_at,
    }
}

#[utoipa::path(
    get,
    path = "/projects/{project}/git/https-credentials",
    operation_id = "listProjectGitHttpsCredentials",
    tag = "projects",
    summary = "List Credentials for Git over HTTPS",
    description = "List the HTTPS credentials for cloning git repositories in this project, one per host. Read the host, user name and last modified time from the response. Passwords never appear in the response.",
    params(ProjectPath),
    responses((status = OK, description = "Saved Credentials", body = Vec<GitHttpsCredentialResponse>), WebError)
)]
pub async fn list_project_git_https_credentials(
    ProjectScope { project, db, .. }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitHttpsCredentialResponse>>>> {
    let rows = gradient_db::project_git_https_credentials::list(&db.web_db, project.id).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(project_git_https_credential_row_to_response)
            .collect(),
    ))
}

#[utoipa::path(
    put,
    path = "/projects/{project}/git/https-credentials",
    operation_id = "saveProjectGitHttpsCredential",
    tag = "projects",
    summary = "Save Credentials for Git over HTTPS",
    description = "Create or replace the credentials for a host. Use a bare hostname for the host, with an optional port (for example `github.com` or `git.example:8443`). IPv6 addresses are invalid. Gradient does password encryption before storage. These credentials apply to every clone of a repository over HTTPS from that host. Gradient does not accept an empty username or password. Requests need the `manageSshKey` permission on the project. Projects in declarative state reject this request.",
    params(ProjectPath),
    request_body = SaveGitHttpsCredentialRequestBody,
    responses((status = OK, description = "Credentials saved", body = GitHttpsCredentialResponse), WebError)
)]
pub async fn save_project_git_https_credential_for_host(
    ProjectScope {
        project, user, db, ..
    }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<SaveGitHttpsCredentialRequestBody>,
) -> WebResult<Json<BaseResponse<GitHttpsCredentialResponse>>> {
    let host = normalize_bare_https_git_host(&body.host).ok_or_else(|| {
        ProjectRefusal::GitCredentialHostIsNotBareHost {
            host: body.host.clone(),
        }
    })?;
    if body.username.trim().is_empty() || body.password.trim().is_empty() {
        return Err(ProjectRefusal::EmptyGitCredentialUserNameOrPassword.into());
    }
    let encrypted =
        gradient_sources::encrypt_secret(&state.config.secrets.crypt_file, &body.password)
            .context("encrypt the password for git credentials over HTTPS")?;

    let row = gradient_db::project_git_https_credentials::upsert(
        &db.web_db,
        project.id,
        &host,
        &body.username,
        encrypted,
        Some(user.id),
    )
    .await?;
    audit
        .write(
            Action::ProjectGitHttpsCredentialCreatedOrReplaced,
            EventOwner::project(project.id),
            serde_json::json!({ "host": row.host, "username": row.username }),
        )
        .await;

    Ok(ok_json(project_git_https_credential_row_to_response(row)))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/git/https-credentials/{credential}",
    operation_id = "deleteProjectGitHttpsCredential",
    tag = "projects",
    summary = "Delete credentials for Git over HTTPS",
    description = "Delete the stored HTTPS credentials. Clones from that host then run without credentials. The `manageSshKey` permission on the project is necessary.",
    params(ProjectGitHttpsCredentialPathParameters),
    responses((status = OK, description = "Credentials deleted", body = bool), WebError)
)]
pub async fn delete_project_git_https_credential_by_id(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<bool>>> {
    let id: ProjectGitHttpsCredentialId = params.param("credential")?;
    let saved = gradient_db::project_git_https_credentials::list(&db.web_db, project.id)
        .await?
        .into_iter()
        .find(|credential| credential.id == id)
        .ok_or_else(|| WebError::not_found(Resource::ProjectGitHttpsCredential))?;
    let deleted =
        gradient_db::project_git_https_credentials::delete(&db.web_db, project.id, saved.id)
            .await?;
    audit
        .write(
            Action::ProjectGitHttpsCredentialDeleted,
            EventOwner::project(project.id),
            serde_json::json!({ "host": saved.host }),
        )
        .await;

    Ok(ok_json(deleted))
}
