/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebResult;
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Readable, Requires, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_sources::{format_cache_key, format_cache_public_key};
use gradient_types::*;
use std::sync::Arc;

pub async fn get_cache_key(
    CacheScope { cache, .. }: CacheScope<Requires<can::ManageCacheKeys>>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let cache_key = format_cache_key(
        &state.config.secrets.crypt_file,
        cache,
        state.config.server.serve_url.clone(),
    )
    .context("generate cache key")?;

    Ok(ok_json(cache_key))
}

pub async fn get_cache_public_key(
    CacheScope { cache, .. }: CacheScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let public_key = format_cache_public_key(
        &state.config.secrets.crypt_file,
        cache,
        state.config.server.serve_url.clone(),
    )
    .context("derive public key")?;

    Ok(ok_json(public_key))
}
