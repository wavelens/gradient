/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::generate_api_key;
use crate::error::{WebError, WebResult};
use axum::extract::State;
use axum::{Extension, Json};
use chrono::Utc;
use core::consts::*;
use core::input::{validate_display_name, validate_username};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ApiKeyRequest {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchUserSettingsRequest {
    pub username: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetUserSettingsResponse {
    pub username: String,
    pub name: String,
    pub email: String,
}

pub async fn get(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<UserInfoResponse>>> {
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
) -> WebResult<Json<BaseResponse<String>>> {
    // TODO: Make sure to delete all related data and that cascade is working
    let auser: AUser = user.into();
    auser.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<ListResponse>>> {
    let api_keys = EApi::find()
        .filter(CApi::OwnedBy.eq(user.id))
        .all(&state.db)
        .await?;

    let api_keys: ListResponse = api_keys
        .iter()
        .map(|k| ListItem {
            id: k.id,
            name: k.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: api_keys,
    };

    Ok(Json(res))
}

pub async fn post_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<ApiKeyRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let existing_api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await?;

    if existing_api_key.is_some() {
        return Err(WebError::already_exists("API-Key Name"));
    }

    let api_key = AApi {
        id: Set(Uuid::new_v4()),
        owned_by: Set(user.id),
        name: Set(body.name.clone()),
        key: Set(generate_api_key()),
        last_used_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
    };

    let api_key = api_key.insert(&state.db).await?;

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
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("API-Key"))?;

    let aapi_key: AApi = api_key.into();
    aapi_key.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "API-Key deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_settings(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<GetUserSettingsResponse>>> {
    let res = BaseResponse {
        error: false,
        message: GetUserSettingsResponse {
            username: user.username.clone(),
            name: user.name.clone(),
            email: user.email.clone(),
        },
    };

    Ok(Json(res))
}

pub async fn patch_settings(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<PatchUserSettingsRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    // Prevent modification of state-managed users
    if user.managed {
        return Err(WebError::Forbidden("Cannot modify state-managed user. This user is managed by configuration and cannot be edited through the API.".to_string()));
    }

    let mut auser: AUser = user.into();

    if let Some(username) = body.username {
        if let Err(e) = validate_username(&username) {
            return Err(WebError::invalid_username(e));
        }

        let existing_user = EUser::find()
            .filter(CUser::Username.eq(username.clone()))
            .one(&state.db)
            .await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists("Username"));
        }

        auser.username = Set(username);
    }

    if let Some(name) = body.name {
        if let Err(e) = validate_display_name(&name) {
            return Err(WebError::BadRequest(format!("Invalid name: {}", e)));
        }
        auser.name = Set(name);
    }

    if let Some(email) = body.email {
        let existing_user = EUser::find()
            .filter(CUser::Email.eq(email.clone()))
            .one(&state.db)
            .await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists("Email"));
        }

        auser.email = Set(email);
    }

    auser.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User updated".to_string(),
    };

    Ok(Json(res))
}
