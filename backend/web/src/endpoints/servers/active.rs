/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use core::db::get_server_by_name;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::ActiveModelTrait;
use std::sync::Arc;

pub async fn post_server_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, server)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, server): (MOrganization, MServer) = match get_server_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        server.clone(),
    )
    .await
    {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Server not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    // Prevent activation of state-managed servers
    if server.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot modify state-managed server activation. This server's active state is managed by configuration.".to_string(),
            }),
        ));
    }

    let mut aserver: AServer = server.into();
    aserver.active = Set(true);
    if let Err(e) = aserver.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to enable server: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Server enabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_server_active(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, server)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, server): (MOrganization, MServer) = match get_server_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        server.clone(),
    )
    .await
    {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Server not found".to_string(),
                }),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Database error: {}", e),
                }),
            ));
        }
    };

    // Prevent deactivation of state-managed servers
    if server.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot modify state-managed server activation. This server's active state is managed by configuration.".to_string(),
            }),
        ));
    }

    let mut aserver: AServer = server.into();
    aserver.active = Set(false);
    if let Err(e) = aserver.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to disable server: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Server disabled".to_string(),
    };

    Ok(Json(res))
}
