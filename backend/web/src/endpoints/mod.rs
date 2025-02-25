/*
* SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
*
* SPDX-License-Identifier: AGPL-3.0-only
*/

pub mod auth;
pub mod builds;
pub mod evals;
pub mod orgs;
pub mod projects;
pub mod servers;
pub mod user;

use axum::extract::Json;
use axum::http::StatusCode;
use core::types::BaseResponse;

pub async fn handle_404() -> (StatusCode, Json<BaseResponse<String>>) {
    (
        StatusCode::NOT_FOUND,
        Json(BaseResponse {
            error: true,
            message: "Not Found".to_string(),
        }),
    )
}

pub async fn get_health(
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "200 ALIVE".to_string(),
    };

    Ok(Json(res))
}
