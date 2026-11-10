/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::CachePath;
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Readable, Requires, RequiresUnmanaged, can};
use anyhow::Context;
use axum::Json;
use axum::extract::State;
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_sources::{format_cache_key, format_cache_public_key, generate_signing_key};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use serde::Serialize;
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct RetiredPublicKeyEntry {
    pub public_key: String,
    pub created_at: NaiveDateTime,
    pub retired_at: NaiveDateTime,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CachePublicKeyHistory {
    pub public_key: String,
    pub created_at: NaiveDateTime,
    pub retired: Vec<RetiredPublicKeyEntry>,
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/keys",
    operation_id = "listCachePublicKeys",
    tag = "caches",
    summary = "List public keys of a cache",
    description = "Return the current public key of a cache. The response can also list every retired public key with the dates those keys were used. A narinfo response can contain a signature from the newest retired key until the re-signing of that path. Paths signed before rotation get that signature too.",
    params(CachePath),
    responses((status = OK, description = "Current public keys and retired public keys", body = CachePublicKeyHistory), WebError)
)]
pub async fn get_cache_public_keys_with_retirement_dates(
    CacheScope { cache, .. }: CacheScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<CachePublicKeyHistory>>> {
    let retired = gradient_db::caches::list_retired_signing_keys_for_cache_newest_retirement_first(
        &state.web_db,
        cache.id,
    )
    .await?;
    let created_at = retired
        .first()
        .map_or(cache.created_at, |newest| newest.retired_at);
    let public_key = format_cache_public_key(
        &state.config.secrets.crypt_file,
        cache,
        state.config.server.serve_url.clone(),
    )
    .context("derive public key")?;

    Ok(ok_json(CachePublicKeyHistory {
        public_key,
        created_at,
        retired: retired
            .into_iter()
            .map(|key| RetiredPublicKeyEntry {
                public_key: key.public_key,
                created_at: key.created_at,
                retired_at: key.retired_at,
            })
            .collect(),
    }))
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/key/rotate",
    operation_id = "rotateCacheKey",
    tag = "caches",
    summary = "Rotate the cache signing key",
    description = "Generate a new signing key pair for the cache. Retire the old key. Existing store paths keep the old signature next to the new signature. Nix clients that still list only the old public key at `trusted-public-keys` can verify those paths until they switch. The signature sweep can re-sign every path with the new key right away. Paths uploaded after rotation carry only the new signature. Both keys use the same key name, so clients trust only one key at a time. Callers need `manageCacheKeys`. Requests for a managed cache fail because keys for that cache come from the state file. Return the new public key.",
    params(CachePath),
    responses((status = OK, description = "New public key in Nix format", body = String, content_type = "application/json"), WebError)
)]
pub async fn rotate_cache_signing_key(
    CacheScope { cache, .. }: CacheScope<RequiresUnmanaged<can::ManageCacheKeys>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<String>>> {
    let crypt_file = &state.config.secrets.crypt_file;
    let serve_url = state.config.server.serve_url.clone();
    let retired_public_key = format_cache_public_key(crypt_file, cache.clone(), serve_url.clone())
        .context("derive public key")?;
    let key_pair = generate_signing_key(crypt_file).context("generate signing key")?;

    let cache =
        gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
            &state.web_db,
            cache,
            retired_public_key.clone(),
            key_pair,
        )
        .await?;
    state.wake.run_signature_sweep_now.notify_one();

    audit
        .write(
            Action::CacheKeyRotate,
            EventOwner::cache(cache.id),
            serde_json::json!({ "retired_public_key": retired_public_key }),
        )
        .await;
    let public_key =
        format_cache_public_key(crypt_file, cache, serve_url).context("derive public key")?;
    Ok(ok_json(public_key))
}
