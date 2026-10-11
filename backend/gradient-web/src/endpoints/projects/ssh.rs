/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{Member, ProjectScope, RequiresUnmanaged, can};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_sources::{format_public_key, generate_ssh_key};
use gradient_types::*;
use sea_orm::ActiveModelTrait;
use sea_orm::ActiveValue::Set;
use std::sync::Arc;

pub async fn get_project_ssh(
    ProjectScope { project, .. }: ProjectScope<Member>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    Ok(ok_json(format_public_key(
        project,
        &state.config.server.serve_url,
    )))
}

pub async fn post_project_ssh(
    ProjectScope { project, .. }: ProjectScope<RequiresUnmanaged<can::ManageSshKey>>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (private_key, public_key) =
        generate_ssh_key(&state.config.secrets.crypt_file).map_err(|e| {
            tracing::error!(error = %e, "Failed to generate SSH key");
            WebError::failed_ssh_key_generation()
        })?;

    let mut aproject: AProject = project.into();

    aproject.private_key = Set(private_key.clone());
    aproject.public_key = Set(public_key.clone());
    let project = aproject.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(project, &state.config.server.serve_url),
    };

    Ok(Json(res))
}
