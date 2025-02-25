/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::consts::*;
use core::database::{add_features, get_organization_by_name, get_server_by_name};
use core::executer::connect;
use core::input::check_index_name;
use core::sources::decrypt_ssh_private_key;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeServerRequest {
    pub name: String,
    pub display_name: String,
    pub host: String,
    pub port: i32,
    pub username: String,
    pub architectures: Vec<String>,
    pub features: Vec<String>,
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Implement pagination
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let servers = EServer::find()
        .filter(CServer::Organization.eq(organization.id))
        .all(&state.db)
        .await
        .unwrap();

    let servers: ListResponse = servers
        .iter()
        .map(|s| ListItem {
            id: s.id,
            name: s.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: servers,
    };

    Ok(Json(res))
}

pub async fn post(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<MakeServerRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Server Name".to_string(),
            }),
        ));
    }

    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let server = EServer::find()
        .filter(
            Condition::all()
                .add(CServer::Organization.eq(organization.id))
                .add(CServer::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if server.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Server Name already exists".to_string(),
            }),
        ));
    };

    let server = AServer {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        display_name: Set(body.display_name.clone()),
        organization: Set(organization.id),
        host: Set(body.host.clone()),
        port: Set(body.port),
        username: Set(body.username.clone()),
        last_connection_at: Set(*NULL_TIME),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let architectures = body
        .architectures
        .iter()
        .map(|a| entity::server::Architecture::try_from(a.as_str()))
        .filter_map(|a| a.ok())
        .collect::<Vec<entity::server::Architecture>>();

    if architectures.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Architectures".to_string(),
            }),
        ));
    }

    let server = server.insert(&state.db).await.unwrap();
    let server_architecture = architectures
        .iter()
        .map(|a| AServerArchitecture {
            id: Set(Uuid::new_v4()),
            server: Set(server.id),
            architecture: Set(a.clone()),
        })
        .collect::<Vec<AServerArchitecture>>();

    add_features(
        Arc::clone(&state),
        body.features.clone(),
        None,
        Some(server.id),
    )
    .await;

    EServerArchitecture::insert_many(server_architecture)
        .exec(&state.db)
        .await
        .unwrap();

    let res = BaseResponse {
        error: false,
        message: server.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_server(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Path(server): Path<String>,
) -> Result<Json<BaseResponse<MServer>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, server): (MOrganization, MServer) = match get_server_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        server.clone(),
    )
    .await
    {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Server not found".to_string(),
                }),
            ))
        }
    };

    let res = BaseResponse {
        error: false,
        message: server,
    };

    Ok(Json(res))
}

pub async fn delete_server(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Path(server): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, server): (MOrganization, MServer) = match get_server_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        server.clone(),
    )
    .await
    {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Server not found".to_string(),
                }),
            ))
        }
    };

    let server: AServer = server.into();
    server.delete(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Server deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_server_check_connection(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Path(server): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (organization, server): (MOrganization, MServer) = match get_server_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        server.clone(),
    )
    .await
    {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Server not found".to_string(),
                }),
            ))
        }
    };

    let (private_key, public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret.clone(), organization.clone()).unwrap();

    match connect(server, None, public_key, private_key).await {
        Ok(_) => Ok(Json(BaseResponse {
            error: false,
            message: "server connection established".to_string(),
        })),
        Err(e) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(BaseResponse {
                error: true,
                message: format!("server connection failed with error: {}", e),
            }),
        )),
    }
}
