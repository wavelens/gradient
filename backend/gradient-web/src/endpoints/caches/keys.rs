/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::CachePath;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Readable, Requires, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_sources::{format_cache_key, format_cache_public_key};
use gradient_types::*;
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/caches/{cache}/key",
    operation_id = "getCacheKey",
    tag = "caches",
    summary = "Get cache secret signing key",
    description = "Returns the Nix-format secret signing key string for the cache, e.g.:\n`gradient.example.com-mycache:AAAA...` (base64-encoded 64-byte ed25519 keypair)\n\nRequires `manageCacheKeys`. Keep the secret key private - do not add it to `nix.conf`.\nTo configure Nix clients use the public key from `GET /caches/{cache}/public-key` instead.",
    params(CachePath),
    responses((status = OK, description = "Cache secret signing key", body = String, content_type = "application/json"), WebError)
)]
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

#[utoipa::path(
    get,
    path = "/caches/{cache}/public-key",
    operation_id = "getCachePublicKey",
    tag = "caches",
    summary = "Get cache public key",
    description = "Returns the Nix-format public key string for the cache, e.g.:\n`gradient.example.com-mycache:AAAA...` (base64-encoded 32-byte ed25519 public key)\n\nAdd this value to `trusted-public-keys` in `nix.conf` or NixOS configuration so\nNix clients can verify signatures on store paths served from this cache.",
    params(CachePath),
    responses((status = OK, description = "Cache public key", body = String, content_type = "application/json"), WebError)
)]
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
