/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::db::get_organization_by_name;
use core::sources::{format_public_key, generate_ssh_key};
use core::types::*;
use sea_orm::ActiveModelTrait;
use sea_orm::ActiveValue::Set;
use std::sync::Arc;

pub async fn get_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization, &state.cli.serve_url),
    };

    Ok(Json(res))
}

pub async fn post_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if organization.managed {
        return Err(WebError::Forbidden(
            "Cannot regenerate SSH key for state-managed organization. This organization is managed by configuration and cannot have its SSH key modified through the API.".to_string(),
        ));
    }

    let (private_key, public_key) =
        generate_ssh_key(state.cli.crypt_secret_file.clone()).map_err(|e| {
            tracing::error!("Failed to generate SSH key: {}", e);
            WebError::failed_ssh_key_generation()
        })?;

    let mut aorganization: AOrganization = organization.into();

    aorganization.private_key = Set(private_key.clone());
    aorganization.public_key = Set(public_key.clone());
    let organization = aorganization.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization, &state.cli.serve_url),
    };

    Ok(Json(res))
}
