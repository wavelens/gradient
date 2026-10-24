/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::resource::Resource;
use crate::store_paths::original_text;
use anyhow::Context;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use gradient_core::ServerState;
use gradient_sources::CacheSigner;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NixRealisation {
    pub out_path: String,
    pub signatures: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/cache/{cache}/build-trace-v2/{drv}/{output}",
    operation_id = "getRealisation",
    tag = "nix-cache",
    summary = "Realisation",
    description = "The output path a build realised for an output of a content-addressed derivation, signed with the cache key. `{drv}` is the derivation base name and `{output}` the output name with a `.doi` suffix.",
    params(("cache" = String, Path), ("drv" = String, Path), ("output" = String, Path)),
    responses((status = OK, description = "Realisation", body = NixRealisation), WebError)
)]
pub async fn realisation(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, drv, output)): Path<(String, String, String)>,
) -> WebResult<Json<NixRealisation>> {
    let output = output
        .strip_suffix(".doi")
        .ok_or_else(|| WebError::not_found(Resource::Path))?;
    let drv = OriginalStorePath::parse(&drv).map_err(|_| WebError::not_found(Resource::Path))?;

    let client_ip = cache_client_ip(&state.config, &headers, peer);
    let ctx = CacheContext::load(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        &headers,
        client_ip,
        cache,
    )
    .await?;

    let mapper = state.cache_mapper(ctx.cache.id);
    let stored_drv = mapper.inbound(&drv);
    let out_path =
        gradient_db::derivation_outputs::realised_output(&state.web_db, stored_drv.hash(), output)
            .await?
            .and_then(|row| row.store_path())
            .or_not_found(Resource::Path)?;
    require_in_cache(&state, &ctx.cache, &out_path).await?;

    let signer = CacheSigner::from_cache(
        &state.config.secrets.crypt_file,
        &ctx.cache,
        &state.config.server.serve_url,
    )
    .context("load the cache signing key")?;
    let out_path = original_text(&mapper, out_path.base());
    let signature = signer.sign_realisation(&drv.base(), output, &out_path);

    Ok(Json(NixRealisation {
        out_path,
        signatures: vec![signature],
    }))
}

async fn require_in_cache(state: &ServerState, cache: &MCache, path: &StorePath) -> WebResult<()> {
    let cached = gradient_db::cached_paths::by_hash(&state.web_db, path.hash())
        .await?
        .filter(|row| row.is_fully_cached())
        .or_not_found(Resource::Path)?;
    gradient_db::cached_paths::signature_in_cache(&state.web_db, cached.id, cache.id)
        .await?
        .or_not_found(Resource::Path)?;
    Ok(())
}
