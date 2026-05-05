/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::helpers::{OptionExt, ok_json};
use crate::authorization::{generate_api_key, hash_api_key};
use crate::error::{WebError, WebResult};
use axum::extract::{Query, State};
use axum::{Extension, Json};

use gradient_core::types::consts::*;
use gradient_core::types::input::{validate_display_name, validate_username};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct UserInfoResponse {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub superuser: bool,
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
    pub is_oidc: bool,
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UserSearchResult {
    pub id: String,
    pub username: String,
    pub name: String,
}

pub async fn get_search(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<Vec<UserSearchResult>>>> {
    let q = params.get("q").cloned().unwrap_or_default();

    let users = EUser::find()
        .filter(CUser::Username.contains(q.as_str()))
        .limit(10)
        .all(&state.web_db)
        .await?;

    let results = users
        .into_iter()
        .map(|u| UserSearchResult {
            id: u.id.to_string(),
            username: u.username,
            name: u.name,
        })
        .collect();

    Ok(ok_json(results))
}

pub async fn get(
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<UserInfoResponse>>> {
    let user_info = UserInfoResponse {
        id: user.id.to_string(),
        username: user.username.clone(),
        name: user.name.clone(),
        email: user.email.clone(),
        superuser: user.superuser,
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
    auser.delete(&state.web_db).await?;

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
        .all(&state.web_db)
        .await?;

    let api_keys: ListResponse = api_keys
        .iter()
        .map(|k| ListItem {
            id: k.id,
            name: k.name.clone(),
            managed: k.managed,
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
        .one(&state.web_db)
        .await?;

    if existing_api_key.is_some() {
        return Err(WebError::already_exists("API-Key Name"));
    }

    let raw_key = generate_api_key();
    let api_key = AApi {
        id: Set(ApiId::now_v7()),
        owned_by: Set(user.id),
        name: Set(body.name.clone()),
        key: Set(hash_api_key(&raw_key)),
        last_used_at: Set(*NULL_TIME),
        created_at: Set(gradient_core::types::now()),
        managed: Set(false),
    };

    api_key.insert(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: format!("GRAD{}", raw_key),
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
        .one(&state.web_db)
        .await?
        .or_not_found("API-Key")?;

    if api_key.managed {
        return Err(WebError::forbidden(
            "Cannot delete a state-managed API key.".to_string(),
        ));
    }

    let aapi_key: AApi = api_key.into();
    aapi_key.delete(&state.web_db).await?;

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
            is_oidc: user.password.is_none(),
            managed: user.managed,
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
        return Err(WebError::forbidden("Cannot modify state-managed user. This user is managed by configuration and cannot be edited through the API."));
    }

    // OIDC users cannot edit their profile — identity is managed by the provider
    if user.password.is_none() {
        return Err(WebError::forbidden("Cannot modify profile of an OIDC user. Your profile is managed by your identity provider."));
    }

    let mut auser: AUser = user.into();

    if let Some(username) = body.username {
        if let Err(e) = validate_username(&username) {
            return Err(WebError::invalid_username(e.to_string()));
        }

        let existing_user = EUser::find()
            .filter(CUser::Username.eq(username.clone()))
            .one(&state.web_db)
            .await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists("Username"));
        }

        auser.username = Set(username);
    }

    if let Some(name) = body.name {
        if let Err(e) = validate_display_name(&name) {
            return Err(WebError::bad_request(format!("Invalid name: {}", e)));
        }
        auser.name = Set(name);
    }

    if let Some(email) = body.email {
        let existing_user = EUser::find()
            .filter(CUser::Email.eq(email.clone()))
            .one(&state.web_db)
            .await?;

        if existing_user.is_some() {
            return Err(WebError::already_exists("Email"));
        }

        auser.email = Set(email);
    }

    auser.update(&state.web_db).await?;

    let res = BaseResponse {
        error: false,
        message: "User updated".to_string(),
    };

    Ok(Json(res))
}
