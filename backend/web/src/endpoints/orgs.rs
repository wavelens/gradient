/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::database::get_organization_by_name;
use core::input::check_index_name;
use core::sources::{format_public_key, generate_ssh_key};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeOrganizationRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Implement pagination
    let organizations = EOrganization::find()
        .filter(COrganization::CreatedBy.eq(user.id))
        .all(&state.db)
        .await
        .unwrap();

    let organizations: ListResponse = organizations
        .iter()
        .map(|o| ListItem {
            id: o.id,
            name: o.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: organizations,
    };

    Ok(Json(res))
}

pub async fn post(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeOrganizationRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Organization Name".to_string(),
            }),
        ));
    }

    let organization = get_organization_by_name(state.0.clone(), user.id, body.name.clone()).await;

    if organization.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Organization Name already exists".to_string(),
            }),
        ));
    }

    let (private_key, public_key) =
        generate_ssh_key(state.cli.crypt_secret_file.clone()).map_err(|e| {
            println!("{}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: "Failed to generate SSH key".to_string(),
                }),
            )
        })?;

    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        use_nix_store: Set(true),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let organization = organization.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<MOrganization>>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
}

pub async fn delete_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<MOrganization>>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
}

pub async fn get_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}

pub async fn post_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let (private_key, public_key) = generate_ssh_key(state.cli.crypt_secret_file.clone()).unwrap();

    let mut aorganization: AOrganization = organization.into();

    aorganization.private_key = Set(private_key.clone());
    aorganization.public_key = Set(public_key.clone());
    let organization = aorganization.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}
