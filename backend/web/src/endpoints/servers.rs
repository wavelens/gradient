/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::consts::*;
use core::database::{add_features, get_organization_by_name, get_server_by_name};
use core::executer::connect;
use core::input::{check_index_name, validate_display_name};
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

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchServerRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub username: Option<String>,
    pub architectures: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<ListResponse>>> {
    // TODO: Implement pagination
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let servers = EServer::find()
        .filter(CServer::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

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

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<MakeServerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Server Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
    }

    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let server = EServer::find()
        .filter(
            Condition::all()
                .add(CServer::Organization.eq(organization.id))
                .add(CServer::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await?;

    if server.is_some() {
        return Err(WebError::already_exists("Server Name"));
    }

    let server = AServer {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        display_name: Set(body.display_name.clone()),
        organization: Set(organization.id),
        active: Set(true),
        host: Set(body.host.clone()),
        port: Set(body.port),
        username: Set(body.username.clone()),
        last_connection_at: Set(*NULL_TIME),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
    };

    let architectures = body
        .architectures
        .iter()
        .map(|a| entity::server::Architecture::try_from(a.as_str()))
        .filter_map(|a| a.ok())
        .collect::<Vec<entity::server::Architecture>>();

    if architectures.is_empty() {
        return Err(WebError::BadRequest("Invalid Architectures".to_string()));
    }

    let server = server.insert(&state.db).await?;
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
        .await?;

    let res = BaseResponse {
        error: false,
        message: server.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_server(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, server)): Path<(String, String)>,
) -> Result<Json<BaseResponse<MServer>>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let res = BaseResponse {
        error: false,
        message: server,
    };

    Ok(Json(res))
}

pub async fn patch_server(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, server)): Path<(String, String)>,
    Json(body): Json<PatchServerRequest>,
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

    // Prevent modification of state-managed servers
    if server.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot modify state-managed server. This server is managed by configuration and cannot be edited through the API.".to_string(),
            }),
        ));
    }

    let mut aserver: AServer = server.into();

    if let Some(name) = body.name.clone() {
        if check_index_name(name.as_str()).is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Server Name".to_string(),
                }),
            ));
        }

        let server = EServer::find()
            .filter(
                Condition::all()
                    .add(CServer::Organization.eq(organization.id))
                    .add(CServer::Name.eq(name.clone())),
            )
            .one(&state.db)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: format!("Database error: {}", e),
                    }),
                )
            })?;

        if server.is_some() {
            return Err((
                StatusCode::CONFLICT,
                Json(BaseResponse {
                    error: true,
                    message: "Server Name already exists".to_string(),
                }),
            ));
        };

        aserver.name = Set(name);
    }

    if let Some(display_name) = body.display_name.clone() {
        if let Err(e) = validate_display_name(&display_name) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: format!("Invalid display name: {}", e),
                }),
            ));
        }
        aserver.display_name = Set(display_name);
    }

    if let Some(host) = body.host.clone() {
        aserver.host = Set(host);
    }

    if let Some(port) = body.port {
        aserver.port = Set(port);
    }

    if let Some(username) = body.username.clone() {
        aserver.username = Set(username);
    }

    if let Some(architectures) = body.architectures.clone() {
        let architectures = architectures
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

        let server_architecture = architectures
            .iter()
            .map(|a| AServerArchitecture {
                id: Set(Uuid::new_v4()),
                server: aserver.id.clone(),
                architecture: Set(a.clone()),
            })
            .collect::<Vec<AServerArchitecture>>();

        if let Err(e) = EServerArchitecture::insert_many(server_architecture)
            .exec(&state.db)
            .await
        {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to insert server architectures: {}", e),
                }),
            ));
        }
    }

    if let Some(features) = body.features.clone() {
        let server_id = match aserver.id.clone().into_value() {
            Some(id) => match id.as_ref_uuid() {
                Some(uuid) => *uuid,
                None => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(BaseResponse {
                            error: true,
                            message: "Invalid server ID format".to_string(),
                        }),
                    ));
                }
            },
            None => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(BaseResponse {
                        error: true,
                        message: "Server ID not found".to_string(),
                    }),
                ));
            }
        };

        add_features(Arc::clone(&state), features, None, Some(server_id)).await;
    }

    if let Err(e) = aserver.update(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to update server: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Server updated".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_server(
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

    // Prevent deletion of state-managed servers
    if server.managed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Cannot delete state-managed server. This server is managed by configuration and cannot be deleted through the API.".to_string(),
            }),
        ));
    }

    let server: AServer = server.into();
    if let Err(e) = server.delete(&state.db).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: format!("Failed to delete server: {}", e),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Server deleted".to_string(),
    };

    Ok(Json(res))
}

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

    let (private_key, public_key) =
        match decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization.clone()) {
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
