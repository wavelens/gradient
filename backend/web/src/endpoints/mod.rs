/*
* SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

pub mod auth;
pub mod builds;
pub mod caches;
pub mod commits;
pub mod evals;
pub mod orgs;
pub mod projects;
pub mod servers;
pub mod user;

use crate::error::{WebError, WebResult};
use axum::extract::{Json, State};
use core::types::{BaseResponse, ServerState};
use serde::Serialize;
use std::sync::Arc;

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
    pub oidc_enabled: bool,
    pub registration_disabled: bool,
    pub email_verification_enabled: bool,
}

pub async fn get_config(
    State(state): State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<ServerConfig>>> {
    let res = BaseResponse {
        error: false,
        message: ServerConfig {
            oidc_enabled: state.cli.oidc_enabled,
            registration_disabled: state.cli.disable_registration || state.cli.oidc_required,
            email_verification_enabled: state.cli.email_enabled && state.cli.email_require_verification,
        },
    };

    Ok(Json(res))
}
