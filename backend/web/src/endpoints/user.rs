/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::generate_api_key;
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::consts::*;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiKeyRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
}

pub async fn get(
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<UserInfoResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user_info = UserInfoResponse {
        id: user.id.to_string(),
        username: user.username.clone(),
        name: user.name.clone(),
        email: user.email.clone(),
    };

    let res = BaseResponse {
        error: false,
        message: user_info,
    };

    Ok(Json(res))
}

pub async fn delete(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Make sure to delete all related data and that cascade is working
    let auser: AUser = user.into();
    auser.delete(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "User deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<ApiKeyRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if api_key.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "API-Key Name already exists".to_string(),
            }),
        ));
    };

    let api_key = AApi {
        id: Set(Uuid::new_v4()),
        owned_by: Set(user.id),
        name: Set(body.name.clone()),
        key: Set(generate_api_key()),
        last_used_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
    };

    let api_key = api_key.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: format!("GRAD{}", api_key.key),
    };

    Ok(Json(res))
}

pub async fn delete_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<ApiKeyRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    let api_key = match api_key {
        Some(api_key) => api_key,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "API-Key not found".to_string(),
                }),
            ));
        }
    };

    let aapi_key: AApi = api_key.into();
    aapi_key.delete(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: format!("API-Key deleted"),
    };

    Ok(Json(res))
}

// TODO: Implement User Settings; but more important user permissions
pub async fn get_settings(
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<MUser>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}

pub async fn post_settings(
    _state: State<Arc<ServerState>>,
    Extension(_user): Extension<MUser>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}
