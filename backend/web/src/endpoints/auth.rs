/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{encode_jwt, oidc_login_create, oidc_login_verify, update_last_login};
use crate::error::{WebError, WebResult};
use axum::Json;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::http::StatusCode;
use axum::body::Body;
use chrono::Utc;
use core::consts::*;
use core::input::{check_index_name, validate_password};
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
) -> WebResult<Json<BaseResponse<String>>> {
    if state.cli.oidc_required || state.cli.disable_registration {
        return Err(WebError::registration_disabled());
    }

    if check_index_name(body.username.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Username"));
    }

    if !EmailAddress::is_valid(body.email.clone().as_str()) {
        return Err(WebError::invalid_email());
    }

    if let Err(e) = validate_password(&body.password) {
        return Err(WebError::invalid_password(e));
    }

    let existing_user = EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.username.clone()))
                .add(CUser::Email.eq(body.email.clone())),
        )
        .one(&state.db)
        .await?;

    if existing_user.is_some() {
        return Err(WebError::already_exists("User"));
    }

    let user = AUser {
        id: Set(Uuid::new_v4()),
        username: Set(body.username.clone()),
        name: Set(body.name.clone()),
        email: Set(body.email.clone()),
        password: Set(Some(generate_hash(body.password.clone()))),
        last_login_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
    };

    let user = user.insert(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: user.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn post_basic_login(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeLoginRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if state.cli.oidc_required {
        return Err(WebError::oauth_required());
    }

    let user = EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.loginname.clone()))
                .add(CUser::Email.eq(body.loginname.clone())),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::invalid_credentials())?;

    let user_password = user
        .password
        .clone()
        .ok_or_else(|| WebError::oauth_required())?;

    verify_password(body.password, &user_password).map_err(|_| WebError::invalid_credentials())?;

    let token =
        encode_jwt(state.clone(), user.id).map_err(|_| WebError::failed_to_generate_token())?;

    update_last_login(state, user)
        .await
        .map_err(|_| WebError::failed_to_update_user())?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn get_oauth_authorize(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let code = query
        .get("code")
        .ok_or_else(|| WebError::invalid_oauth_code())?;

    let user: MUser = oidc_login_verify(state.clone(), code.to_string())
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?;

    let token = encode_jwt(state, user.id).map_err(|_| WebError::failed_to_generate_token())?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_oauth_authorize(
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.cli.oidc_enabled {
        return Err(WebError::oauth_disabled());
    }

    let authorize_url = oidc_login_create(state)
        .await
        .map_err(|e| WebError::Unauthorized(e.to_string()))?;

    let res = BaseResponse {
        error: false,
        message: authorize_url.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_oidc_login(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    if !state.cli.oidc_enabled {
        return Err(WebError::oauth_disabled());
    }

    let authorize_url = oidc_login_create(state)
        .await
        .map_err(|e| WebError::Unauthorized(e.to_string()))?;

    Ok(Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", authorize_url.to_string())
        .body(Body::empty())
        .unwrap())
}

pub async fn get_oidc_callback(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let code = query
        .get("code")
        .ok_or_else(|| WebError::invalid_oauth_code())?;

    let user: MUser = oidc_login_verify(state.clone(), code.to_string())
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?;

    let token = encode_jwt(state, user.id).map_err(|_| WebError::failed_to_generate_token())?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_logout(_state: State<Arc<ServerState>>) -> WebResult<Json<BaseResponse<String>>> {
    // TODO: invalidate token if needed
    let res = BaseResponse {
        error: false,
        message: "Logout Successfully".to_string(),
    };

    Ok(Json(res))
}
