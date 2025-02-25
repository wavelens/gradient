/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{encode_jwt, oauth_login_create, oauth_login_verify, update_last_login};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use core::consts::*;
use core::input::check_index_name;
use core::types::*;
use email_address::EmailAddress;
use password_auth::{generate_hash, verify_password};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeUserRequest {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password: String,
}

pub async fn post_basic_register(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeUserRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if state.cli.oauth_required || state.cli.disable_registration {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Registration is disabled".to_string(),
            }),
        ));
    }

    if check_index_name(body.username.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Username".to_string(),
            }),
        ));
    }

    if !EmailAddress::is_valid(body.email.clone().as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Email".to_string(),
            }),
        ));
    }

    let user = EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.username.clone()))
                .add(CUser::Email.eq(body.email.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if user.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "User already exists".to_string(),
            }),
        ));
    };

    let user = AUser {
        id: Set(Uuid::new_v4()),
        username: Set(body.username.clone()),
        name: Set(body.name.clone()),
        email: Set(body.email.clone()),
        password: Set(Some(generate_hash(body.password.clone()))),
        last_login_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
    };

    let user = user.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: user.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn post_basic_login(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeLoginRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if state.cli.oauth_required {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(BaseResponse {
                error: true,
                message: "Please login via OAuth".to_string(),
            }),
        ));
    }

    let user = match EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.loginname.clone()))
                .add(CUser::Email.eq(body.loginname.clone())),
        )
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(u) => u,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid credentials".to_string(),
                }),
            ))
        }
    };

    let user_password = match user.password.clone() {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "Please login via OAuth".to_string(),
                }),
            ))
        }
    };

    verify_password(body.password, &user_password).map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            Json(BaseResponse {
                error: true,
                message: "Invalid credentials".to_string(),
            }),
        )
    })?;

    let token = encode_jwt(state.clone(), user.id).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: "Failed to generate token".to_string(),
            }),
        )
    })?;

    update_last_login(state, user).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: "Failed to update user".to_string(),
            }),
        )
    })?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn get_oauth_authorize(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let code = match query.get("code") {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid OAuth Code".to_string(),
                }),
            ))
        }
    };

    let user: MUser = match oauth_login_verify(state.clone(), code.to_string()).await {
        Ok(u) => u,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: e.to_string(),
                }),
            ))
        }
    };

    let token = encode_jwt(state, user.id).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: "Failed to generate token".to_string(),
            }),
        )
    })?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_oauth_authorize(
    state: State<Arc<ServerState>>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if !state.cli.oauth_enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "OAuth login is disabled".to_string(),
            }),
        ));
    }

    let authorize_url = oauth_login_create(state).map_err(|e| {
        (
            StatusCode::UNAUTHORIZED,
            Json(BaseResponse {
                error: true,
                message: e.to_string(),
            }),
        )
    })?;

    let res = BaseResponse {
        error: false,
        message: authorize_url.to_string(),
    };

    Ok(Json(res))
}

pub async fn post_logout(
    _state: State<Arc<ServerState>>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: invalidate token if needed
    let res = BaseResponse {
        error: false,
        message: "Logout Successfully".to_string(),
    };

    Ok(Json(res))
}
