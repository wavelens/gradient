/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use crate::resource::Resource;
use anyhow::Context;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_core::upstream_source::{fetch_upstream_log, substitution_sources};
use gradient_db::WebDb;
use gradient_db::caches::upstream::pull_through_upstream_caches;
use gradient_sources::parse_drv_hash_name;
use gradient_storage::LogStorage;
use std::sync::Arc;

/// A pull-through cache is substituting paths it never built (#547). The upstream caches are asked
/// for the log when this cache does not hold it.
pub async fn log(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, drv)): Path<(String, String)>,
) -> WebResult<Response> {
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

    if let Some(body) = local_log(&state.web_db, &state.log_storage, &ctx, &drv).await? {
        return log_response(body, "HIT");
    }

    let upstream_caches = pull_through_upstream_caches(&state.web_db, &ctx.cache).await?;
    match fetch_upstream_log(&substitution_sources(&upstream_caches), &drv).await {
        Some(body) => log_response(body, "MISS"),
        None => Err(WebError::not_found(Resource::Log)),
    }
}

/// Failed builds are not excluded on purpose. A rebuild that failed after the first success is
/// still carrying the most recent log.
async fn local_log(
    db: &WebDb,
    log_storage: &Arc<dyn LogStorage>,
    ctx: &CacheContext,
    drv: &str,
) -> WebResult<Option<String>> {
    let Ok((drv_hash, drv_name)) = parse_drv_hash_name(drv) else {
        return Ok(None);
    };

    let Some(derivation) = gradient_db::cached_paths::derivation_signed_in_cache(
        db,
        ctx.cache.id,
        &drv_hash,
        &drv_name,
    )
    .await?
    else {
        return Ok(None);
    };

    let Some(shared_build) = gradient_db::derivation_builds::by_derivation(db, derivation).await?
    else {
        return Ok(None);
    };

    let Some(key) =
        gradient_db::scheduling::build_attempt::latest_attempt_id(db, shared_build.id).await?
    else {
        return Ok(None);
    };

    Ok(log_storage
        .read(key)
        .await
        .ok()
        .filter(|body| !body.is_empty()))
}

fn log_response(body: String, cache_status: &'static str) -> WebResult<Response> {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .header("x-cache", HeaderValue::from_static(cache_status))
        .header(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        )
        .body(Body::from(body))
        .context("build response")
        .map_err(Into::into)
}
