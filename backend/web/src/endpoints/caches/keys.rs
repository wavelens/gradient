/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use core::db::get_any_cache_by_name;
use core::sources::{format_cache_key, format_cache_public_key};
use core::types::*;
use std::sync::Arc;

// ── Access helpers ────────────────────────────────────────────────────────────

/// Load a cache visible to `user_id`: owned caches and public caches.
async fn load_cache_for_owner(
    state: &Arc<ServerState>,
    user_id: uuid::Uuid,
    cache_name: String,
) -> WebResult<MCache> {
    let cache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public && cache.created_by != user_id {
        return Err(WebError::not_found("Cache"));
    }

    Ok(cache)
}

/// Load a cache visible to `maybe_user`: public caches, or owned if authenticated.
async fn load_cache_readable(
    state: &Arc<ServerState>,
    maybe_user: &Option<MUser>,
    cache_name: String,
) -> WebResult<MCache> {
    let cache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let allowed = cache.public || matches!(maybe_user, Some(u) if u.id == cache.created_by);
    if !allowed {
        return Err(WebError::not_found("Cache"));
    }

    Ok(cache)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn get_cache_key(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache_for_owner(&state, user.id, cache).await?;

    let cache_key = format_cache_key(
        state.cli.crypt_secret_file.clone(),
        cache,
        state.cli.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!("Failed to generate cache key: {}", e);
        WebError::InternalServerError("Failed to generate cache key".to_string())
    })?;

    Ok(Json(BaseResponse {
        error: false,
        message: cache_key,
    }))
}

pub async fn get_cache_public_key(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache_readable(&state, &maybe_user, cache).await?;

    let public_key = format_cache_public_key(
        state.cli.crypt_secret_file.clone(),
        cache,
        state.cli.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!("Failed to derive public key: {}", e);
        WebError::InternalServerError("Failed to derive public key".to_string())
    })?;

    Ok(Json(BaseResponse {
        error: false,
        message: public_key,
    }))
}

