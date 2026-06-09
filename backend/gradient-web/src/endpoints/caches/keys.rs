/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::CachePermission;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_core::sources::{format_cache_key, format_cache_public_key};
use gradient_core::types::*;
use std::sync::Arc;

pub async fn get_cache_key(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache,
        CacheAccess::Require {
            permission: CachePermission::ManageCacheKeys,
            reject_managed: false,
        },
    )
    .await?;

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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

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
