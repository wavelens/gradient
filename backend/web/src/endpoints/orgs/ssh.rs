/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, OrgAccess, load_org};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::sources::{format_public_key, generate_ssh_key};
use gradient_core::types::*;
use sea_orm::ActiveModelTrait;
use sea_orm::ActiveValue::Set;
use std::sync::Arc;

pub async fn get_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        organization,
        OrgAccess::Member {
            reject_managed: false,
        },
    )
    .await?;

    Ok(ok_json(format_public_key(
        organization,
        &state.config.server.serve_url,
    )))
}

pub async fn post_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageSshKey,
            reject_managed: true,
        },
    )
    .await?;

    let (private_key, public_key) = generate_ssh_key(&state.config.secrets.crypt_secret_file)
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to generate SSH key");
            WebError::failed_ssh_key_generation()
        })?;

    let mut aorganization: AOrganization = organization.into();

    aorganization.private_key = Set(private_key.clone());
    aorganization.public_key = Set(public_key.clone());
    let organization = aorganization.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization, &state.config.server.serve_url),
    };

    Ok(Json(res))
}
