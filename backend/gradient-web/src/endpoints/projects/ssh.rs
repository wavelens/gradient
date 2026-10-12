/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::ProjectPath;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{Member, ProjectScope, RequiresUnmanaged, can};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_sources::{format_public_key, generate_ssh_key};
use gradient_types::*;
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/projects/{project}/ssh",
    operation_id = "getProjectSsh",
    tag = "projects",
    summary = "Get SSH public key",
    description = "Returns the SSH public key for the project. This key must be added to\neach remote build server's `authorized_keys` to allow Gradient to connect.",
    params(ProjectPath),
    responses((status = OK, description = "SSH public key", body = String, content_type = "application/json"), WebError)
)]
pub async fn get_project_ssh(
    ProjectScope { project, .. }: ProjectScope<Member>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    Ok(ok_json(format_public_key(
        project,
        &state.config.server.serve_url,
    )))
}

#[utoipa::path(
    post,
    path = "/projects/{project}/ssh",
    operation_id = "postProjectSsh",
    tag = "projects",
    summary = "Regenerate SSH key pair",
    description = "Generates a new Ed25519 key pair for the project. The previous key will stop working on remote servers immediately.",
    params(ProjectPath),
    responses((status = OK, description = "New SSH public key", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_project_ssh(
    ProjectScope { project, .. }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (private_key, public_key) =
        generate_ssh_key(&state.config.secrets.crypt_file).map_err(|e| {
            tracing::error!(error = %e, "Failed to generate SSH key");
            WebError::failed_ssh_key_generation()
        })?;

    let project =
        gradient_db::projects::replace_ssh_key(&state.web_db, project, private_key, public_key)
            .await?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(project, &state.config.server.serve_url),
    };

    Ok(Json(res))
}
