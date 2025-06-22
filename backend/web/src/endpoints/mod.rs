/*
* SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
use axum::extract::Json;
use core::types::BaseResponse;

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
