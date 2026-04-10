/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::db::get_server_by_name;
use core::executer::connect;
use core::sources::decrypt_ssh_private_key;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, EntityTrait};
use std::sync::Arc;

pub async fn post_server_check_connection(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, server)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (organization, server): (MOrganization, MServer) = match get_server_by_name(
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

    let (private_key, public_key) = match decrypt_ssh_private_key(
        state.cli.crypt_secret_file.clone(),
        organization.clone(),
        &state.cli.serve_url,
    ) {
        Ok(keys) => keys,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to decrypt SSH private key: {}", e),
                }),
            ));
        }
    };

    let server_id = server.id;
    match connect(server, None, public_key, private_key).await {
        Ok(_) => {
            if let Ok(Some(s)) = EServer::find_by_id(server_id).one(&state.db).await {
                let mut aserver: AServer = s.into();
                aserver.last_connection_at = Set(Utc::now().naive_utc());
                let _ = aserver.update(&state.db).await;
            }
            Ok(Json(BaseResponse {
                error: false,
                message: "server connection established".to_string(),
            }))
        }
        Err(e) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(BaseResponse {
                error: true,
                message: format!("server connection failed with error: {}", e),
            }),
        )),
    }
}
