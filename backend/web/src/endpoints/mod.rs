/*
* SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

pub mod auth;
pub mod badges;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod evals;
pub mod forge_hooks;
pub mod orgs;
pub mod projects;
pub mod servers;
pub mod stats;
pub mod user;
pub mod webhooks;

use crate::error::{WebError, WebResult};
use axum::extract::{Json, State};
use core::types::{BaseResponse, ServerState};
use core::types::{COrganizationUser, EOrganizationUser};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

pub async fn user_is_org_member(
    state: &Arc<ServerState>,
    user_id: Uuid,
    organization_id: Uuid,
) -> Result<bool, WebError> {
    Ok(EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.db)
        .await?
        .is_some())
}

pub async fn handle_404() -> WebError {
    WebError::NotFound("Not Found".to_string())
}

pub async fn get_health() -> WebResult<Json<BaseResponse<String>>> {
    let res = BaseResponse {
        error: false,
        message: "200 ALIVE".to_string(),
    };

    Ok(Json(res))
}

#[derive(Serialize)]
pub struct ServerConfig {
    pub version: String,
    pub oidc_enabled: bool,
    pub registration_enabled: bool,
    pub email_verification_enabled: bool,
    /// Whether the server advertises HTTP/3 (QUIC) support.
    /// Clients may attempt an HTTP/3 upgrade when this is true.
    /// Actual HTTP/3 termination is handled by the reverse proxy (nginx).
    pub quic: bool,
}

pub async fn get_config(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<ServerConfig>>> {
    let res = BaseResponse {
        error: false,
        message: ServerConfig {
            version: env!("CARGO_PKG_VERSION").to_string(),
            oidc_enabled: state.cli.oidc_enabled,
            registration_enabled: state.cli.enable_registration && !state.cli.oidc_required,
            email_verification_enabled: state.cli.email_enabled
                && state.cli.email_require_verification,
            quic: state.cli.quic,
        },
    };

    Ok(Json(res))
}
