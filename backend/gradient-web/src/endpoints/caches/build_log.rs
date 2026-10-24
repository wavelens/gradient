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
use gradient_core::build_logs::{BuildLog, find_build_log};
use gradient_types::OriginalStorePath;
use std::sync::Arc;

/// A pull-through cache is substituting paths it never built (#547). The upstream caches are asked
/// for the log when this cache does not hold it.
#[utoipa::path(
    get,
    path = "/cache/{cache}/log/{drv}",
    operation_id = "getCacheBuildLog",
    tag = "nix-cache",
    summary = "Build log",
    description = "Build log read by `nix log`. Serves this cache's own log, otherwise the first upstream hit. `X-Cache` reports `HIT` or `MISS`.",
    params(("cache" = String, Path), ("drv" = String, Path)),
    responses((status = OK, description = "Build log", body = String, content_type = "text/plain"), WebError)
)]
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

    let not_found = || WebError::not_found(Resource::Log);
    let drv = OriginalStorePath::parse(&drv).map_err(|_| not_found())?;
    let found = find_build_log(
        &state.web_db,
        state.log_storage.as_ref(),
        std::slice::from_ref(&ctx.cache),
        &state.cache_mapper(ctx.cache.id),
        &drv,
    )
    .await?;

    match found {
        Some(BuildLog::Stored(body)) => log_response(body, "HIT"),
        Some(BuildLog::Upstream(body)) => log_response(body, "MISS"),
        None => Err(not_found()),
    }
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
