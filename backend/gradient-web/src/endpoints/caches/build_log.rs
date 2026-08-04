/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_sources::parse_drv_hash_name;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

/// `GET /cache/{cache}/log/{drv}` - the build log `nix log` asks a binary cache
/// for.
///
/// Serves our own log when this cache holds the derivation, and otherwise asks
/// the cache's upstreams: a pull-through cache substitutes paths it never built,
/// and reporting those logs as missing is the whole of #547. `X-Cache` reports
/// which of the two happened.
pub async fn log(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, drv)): Path<(String, String)>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;

    if let Some(body) = local_log(&state, &ctx, &drv).await? {
        return log_response(body, "HIT");
    }

    let upstreams = upstream_urls(&state, ctx.cache.id).await;
    match fetch_log_from_upstreams(&state.http, &upstreams, &drv).await {
        Some(body) => log_response(body, "MISS"),
        None => Err(WebError::not_found("Log")),
    }
}

/// This cache's own log for `drv`, if it holds the derivation and an attempt
/// produced any output. Deliberately not restricted to successful builds - a
/// failed build's log is the one most worth reading.
async fn local_log(
    state: &Arc<ServerState>,
    ctx: &CacheContext,
    drv: &str,
) -> WebResult<Option<String>> {
    let Ok((drv_hash, drv_name)) = parse_drv_hash_name(drv) else {
        return Ok(None);
    };

    let Some(derivation_row) = EDerivation::find()
        .filter(CDerivation::Hash.eq(drv_hash))
        .filter(CDerivation::Name.eq(drv_name))
        .one(&state.web_db)
        .await?
    else {
        return Ok(None);
    };

    let linked = ECacheDerivation::find()
        .filter(CCacheDerivation::Cache.eq(ctx.cache.id))
        .filter(CCacheDerivation::Derivation.eq(derivation_row.id))
        .one(&state.web_db)
        .await?
        .is_some();
    if !linked {
        return Ok(None);
    }

    let Some(anchor) = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.eq(derivation_row.id))
        .one(&state.web_db)
        .await?
    else {
        return Ok(None);
    };

    let Some(key) = gradient_db::latest_attempt_id(&state.web_db, anchor.id).await? else {
        return Ok(None);
    };

    Ok(state
        .log_storage
        .read(key)
        .await
        .ok()
        .filter(|body| !body.is_empty()))
}

async fn upstream_urls(state: &Arc<ServerState>, cache: CacheId) -> Vec<String> {
    ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|upstream| upstream.url)
        .collect()
}

/// Ask each upstream in turn for `drv`'s build log and return the first hit.
///
/// Logs are plain text and carry no signature, so - unlike narinfos - there is
/// nothing to verify; an upstream that 404s or errors is skipped.
pub async fn fetch_log_from_upstreams(
    client: &reqwest::Client,
    upstreams: &[String],
    drv: &str,
) -> Option<String> {
    for base_url in upstreams {
        let url = format!("{}/log/{}", base_url.trim_end_matches('/'), drv);
        let Ok(response) = client.get(&url).send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        match response.text().await {
            Ok(body) if !body.is_empty() => return Some(body),
            _ => continue,
        }
    }

    None
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
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}
