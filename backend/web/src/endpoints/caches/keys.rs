/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_core::db::get_any_cache_by_name;
use gradient_core::sources::{format_cache_key, format_cache_public_key};
use gradient_core::types::*;
use std::sync::Arc;

// ── Access helpers ────────────────────────────────────────────────────────────

/// Load a cache visible to `user_id`: owned caches and public caches.
async fn load_cache_for_owner(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_name: String,
) -> WebResult<MCache> {
    let cache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .or_not_found("Cache")?;

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
        .or_not_found("Cache")?;

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
        &state.config.secrets.crypt_secret_file,
        cache,
        state.config.server.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to generate cache key");
        WebError::internal("Failed to generate cache key")
    })?;

    Ok(ok_json(cache_key))
}

pub async fn get_cache_public_key(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache_readable(&state, &maybe_user, cache).await?;

    let public_key = format_cache_public_key(
        &state.config.secrets.crypt_secret_file,
        cache,
        state.config.server.serve_url.clone(),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to derive public key");
        WebError::internal("Failed to derive public key")
    })?;

    Ok(ok_json(public_key))
}
