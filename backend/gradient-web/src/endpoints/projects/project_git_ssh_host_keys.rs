/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::ProjectRefusal;
use crate::api_spec::{ProjectPath, ProjectSshHostKeyPathParameters};
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::json_body::JsonBody;
use crate::resource::Resource;
use crate::scope::{Member, PathParams, ProjectScope, RequiresUnmanaged, can};
use axum::Json;
use gradient_entity::ids::ProjectSshHostKeyId;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use gradient_util::ssh::HOST_KEYS;
use serde::{Deserialize, Serialize};
use ssh_key::{HashAlg, PublicKey};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct GitSshHostKeyResponse {
    pub id: ProjectSshHostKeyId,
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    pub public_key: String,
    pub learned_at: chrono::NaiveDateTime,
    pub changed_at: Option<chrono::NaiveDateTime>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SaveHostKeyRequestBody {
    pub host: String,
    pub port: u16,
    pub public_key: String,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ScannedHostKey {
    pub public_key: String,
    pub fingerprint: String,
}

fn project_git_ssh_host_key_row_to_response(row: MProjectSshHostKey) -> GitSshHostKeyResponse {
    let parsed = PublicKey::from_openssh(&row.public_key).ok();
    GitSshHostKeyResponse {
        id: row.id,
        host: row.host,
        port: u16::try_from(row.port).unwrap_or_default(),
        algorithm: parsed
            .as_ref()
            .map(|key| key.algorithm().as_str().to_owned())
            .unwrap_or_default(),
        fingerprint: parsed
            .as_ref()
            .map(|key| key.fingerprint(HashAlg::Sha256).to_string())
            .unwrap_or_default(),
        public_key: row.public_key,
        learned_at: row.learned_at,
        changed_at: row.changed_at,
    }
}

fn parse_accepted_ssh_host_public_key(public_key: &str) -> WebResult<PublicKey> {
    let key = PublicKey::from_openssh(public_key.trim()).map_err(|e| {
        ProjectRefusal::InvalidSshHostPublicKey {
            reason: e.to_string(),
        }
    })?;
    let algorithm = key.algorithm();
    if !HOST_KEYS
        .iter()
        .any(|accepted| accepted.as_str() == algorithm.as_str())
    {
        return Err(ProjectRefusal::InvalidSshHostPublicKey {
            reason: "Only Ed25519 and ECDSA keys work as host keys".to_owned(),
        }
        .into());
    }

    Ok(key)
}

async fn load_project_git_ssh_host_key(
    db: &impl sea_orm::ConnectionTrait,
    project: ProjectId,
    params: &PathParams,
) -> WebResult<MProjectSshHostKey> {
    let id: ProjectSshHostKeyId = params.param("host_key")?;
    gradient_db::project_ssh_host_keys::one(db, project, id)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::SavedProjectGitServerSshHostKey))
}

#[utoipa::path(
    get,
    path = "/projects/{project}/ssh/host-keys",
    operation_id = "listProjectGitSshHostKeys",
    tag = "projects",
    summary = "List Git host keys",
    description = "List the host key for SSH on each git server that this project can trust. Gradient can store the key of a host on first connection. Gradient can reject the connection on a key change.",
    params(ProjectPath),
    responses((status = OK, description = "Saved host keys", body = Vec<GitSshHostKeyResponse>), WebError)
)]
pub async fn list_project_git_ssh_host_keys(
    ProjectScope { project, db, .. }: ProjectScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<GitSshHostKeyResponse>>>> {
    let rows = gradient_db::project_ssh_host_keys::list(&db.web_db, project.id).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(project_git_ssh_host_key_row_to_response)
            .collect(),
    ))
}

#[utoipa::path(
    put,
    path = "/projects/{project}/ssh/host-keys",
    operation_id = "saveProjectSshHostKey",
    tag = "projects",
    summary = "Save host keys for Git",
    description = "Save the host key for a host and port. Replace the previous key and clear the changed mark. Accept only Ed25519 and ECDSA keys. Grant the `manageSshKey` permission (`manageSshKey`) for the project.",
    params(ProjectPath),
    request_body = SaveHostKeyRequestBody,
    responses((status = OK, description = "Host keys saved", body = GitSshHostKeyResponse), WebError)
)]
pub async fn save_project_ssh_host_key_and_clear_changed_flag(
    ProjectScope { project, db, .. }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
    audit: Audit,
    JsonBody(body): JsonBody<SaveHostKeyRequestBody>,
) -> WebResult<Json<BaseResponse<GitSshHostKeyResponse>>> {
    let host = body.host.trim();
    if host.is_empty() {
        return Err(ProjectRefusal::EmptyGitServerHostName.into());
    }
    let key = parse_accepted_ssh_host_public_key(&body.public_key)?;
    let public_key = key
        .to_openssh()
        .map_err(|e| ProjectRefusal::InvalidSshHostPublicKey {
            reason: e.to_string(),
        })?;

    let row = gradient_db::project_ssh_host_keys::upsert(
        &db.web_db,
        project.id,
        host,
        body.port,
        &public_key,
    )
    .await?;
    audit
        .write(
            Action::ProjectGitServerSshHostKeyConfirmedOrReplaced,
            EventOwner::project(project.id),
            serde_json::json!({ "host": row.host, "port": body.port }),
        )
        .await;

    Ok(ok_json(project_git_ssh_host_key_row_to_response(row)))
}

#[utoipa::path(
    post,
    path = "/projects/{project}/ssh/host-keys/{host_key}/scan",
    operation_id = "scanProjectSshHostKey",
    tag = "projects",
    summary = "Read the current host key",
    description = "Grant the `manageSshKey` permission on the project. Connect to the host for the saved key and return the key that the host can offer now, along with its fingerprint. The request does not save a key. Compare the fingerprint with the one that the Git server can publish. Run this comparison before saving a key.",
    params(ProjectSshHostKeyPathParameters),
    responses((status = OK, description = "Current host keys", body = ScannedHostKey), WebError)
)]
pub async fn connect_to_saved_host_and_get_current_key_with_fingerprint(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
) -> WebResult<Json<BaseResponse<ScannedHostKey>>> {
    let saved = load_project_git_ssh_host_key(&db.web_db, project.id, &params).await?;
    let port = u16::try_from(saved.port).unwrap_or_default();
    let read_failed = |reason: String| ProjectRefusal::GitServerSshHostKeyReadFailed { reason };
    let public_key = gradient_sources::fetch_ssh_host_key(&saved.host, port)
        .await
        .map_err(|e| read_failed(e.to_string()))?;
    let fingerprint = PublicKey::from_openssh(&public_key)
        .map_err(|e| read_failed(e.to_string()))?
        .fingerprint(HashAlg::Sha256)
        .to_string();

    Ok(ok_json(ScannedHostKey {
        public_key,
        fingerprint,
    }))
}

#[utoipa::path(
    delete,
    path = "/projects/{project}/ssh/host-keys/{host_key}",
    operation_id = "deleteProjectSshHostKey",
    tag = "projects",
    summary = "Delete a Git host key.",
    description = "Delete a saved host key. Later connections to that host store the key again. Grant the `manageSshKey` permission in the project.",
    params(ProjectSshHostKeyPathParameters),
    responses((status = OK, description = "Host key deleted", body = bool), WebError)
)]
pub async fn delete_project_ssh_host_key_by_id(
    ProjectScope {
        project,
        db,
        params,
        ..
    }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<bool>>> {
    let saved = load_project_git_ssh_host_key(&db.web_db, project.id, &params).await?;
    let deleted =
        gradient_db::project_ssh_host_keys::delete(&db.web_db, project.id, saved.id).await?;
    audit
        .write(
            Action::ProjectGitServerSshHostKeyDeleted,
            EventOwner::project(project.id),
            serde_json::json!({ "host": saved.host, "port": saved.port }),
        )
        .await;

    Ok(ok_json(deleted))
}
