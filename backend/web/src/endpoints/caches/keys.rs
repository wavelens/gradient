/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::user_can_access_cache;
use crate::authorization::{MaybeUser, generate_api_key};
use crate::error::{WebError, WebResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Extension;
use chrono::Utc;
use core::db::get_any_cache_by_name;
use core::sources::{format_cache_key, format_cache_public_key};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

pub async fn get_cache_key(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_any_cache_by_name(state.0.clone(), cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
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

    if !cache.public && cache.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Cache not found".to_string(),
            }),
        ));
    }

    let cache_key = match format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache,
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => key,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to generate cache key: {}", e),
                }),
            ));
        }
    };

    let res = BaseResponse {
        error: false,
        message: cache_key,
    };

    Ok(Json(res))
}

pub async fn get_cache_public_key(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let cache: MCache = match get_any_cache_by_name(state.0.clone(), cache.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
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

    let allowed = cache.public || matches!(&maybe_user, Some(u) if u.id == cache.created_by);
    if !allowed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Cache not found".to_string(),
            }),
        ));
    }

    let public_key = match format_cache_public_key(
        state.cli.crypt_secret_file.clone(),
        cache,
        state.cli.serve_url.clone(),
    ) {
        Ok(key) => key,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: format!("Failed to derive public key: {}", e),
                }),
            ));
        }
    };

    Ok(Json(BaseResponse {
        error: false,
        message: public_key,
    }))
}

/// Returns a `.netrc` snippet for authenticating Nix against this cache.
///
/// Format:
/// ```text
/// machine <host>
/// login gradient
/// password GRAD<api_key>
/// ```
///
/// A dedicated API key named `netrc-<cache>` is created on first call and reused
/// on subsequent calls, so the returned credentials stay stable.
pub async fn get_cache_netrc(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = get_any_cache_by_name(state.0.clone(), cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public && !user_can_access_cache(&state, &cache, &user).await {
        return Err(WebError::not_found("Cache"));
    }

    let key_name = format!("netrc-{}", cache.name);

    let raw_key = match EApi::find()
        .filter(CApi::OwnedBy.eq(user.id))
        .filter(CApi::Name.eq(key_name.clone()))
        .one(&state.db)
        .await?
    {
        Some(existing) => existing.key,
        None => {
            let new_key = generate_api_key();
            AApi {
                id: Set(Uuid::new_v4()),
                owned_by: Set(user.id),
                name: Set(key_name),
                key: Set(new_key.clone()),
                last_used_at: Set(Utc::now().naive_utc()),
                created_at: Set(Utc::now().naive_utc()),
                managed: Set(false),
            }
            .insert(&state.db)
            .await?;
            new_key
        }
    };

    let host = state
        .cli
        .serve_url
        .replace("https://", "")
        .replace("http://", "")
        .split('/')
        .next()
        .unwrap_or("localhost")
        .to_string();

    let netrc = format!(
        "machine {}\nlogin gradient\npassword GRAD{}\n",
        host, raw_key
    );

    Ok(Json(BaseResponse {
        error: false,
        message: netrc,
    }))
}
