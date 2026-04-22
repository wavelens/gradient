/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, get_nar_by_hash};
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use core::sources::{get_hash_from_url, verify_narinfo_signature};
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use tracing::warn;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn text_response(content_type: &'static str, body: String) -> WebResult<Response<String>> {
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(body)
        .map_err(|e| WebError::InternalServerError(format!("Failed to build response: {}", e)))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(cache): Path<String>,
) -> WebResult<Response<String>> {
    let ctx = CacheContext::load(&state, &headers, cache).await?;

    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority: ctx.cache.priority,
    };

    text_response("text/x-nix-cache-info", res.to_nix_string())
}

pub async fn gradient_cache_info(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(cache): Path<String>,
) -> WebResult<Response<String>> {
    CacheContext::load(&state, &headers, cache).await?;

    let body = format!(
        "GradientVersion: {}\nGradientUrl: {}\n",
        env!("CARGO_PKG_VERSION"),
        state.cli.serve_url,
    );

    text_response("text/x-gradient-cache-info", body)
}

pub async fn path(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> WebResult<Response<String>> {
    let path_hash =
        get_hash_from_url(path.clone()).map_err(|e| WebError::BadRequest(e.to_string()))?;

    if !path.ends_with(".narinfo") {
        return Err(WebError::not_found("Path"));
    }

    let ctx = CacheContext::load(&state, &headers, cache).await?;

    if let Ok(path_info) =
        get_nar_by_hash(Arc::clone(&state), ctx.cache.clone(), path_hash.clone()).await
    {
        return text_response("text/x-nix-narinfo", path_info.to_nix_string());
    }

    // Fall back: check external upstream caches.
    let rewritten = fetch_from_upstream(&state, &ctx.cache, &path_hash).await;
    if let Some(body) = rewritten {
        return text_response("text/x-nix-narinfo", body);
    }

    Err(WebError::not_found("Path"))
}

async fn fetch_from_upstream(
    state: &Arc<ServerState>,
    cache: &MCache,
    path_hash: &str,
) -> Option<String> {
    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.db)
        .await
        .unwrap_or_default();

    let http_client = reqwest::Client::new();
    for upstream in upstreams {
        let Some(base_url) = upstream.url.as_deref() else {
            continue;
        };
        let Some(public_key) = upstream.public_key.as_deref() else {
            warn!(upstream = %upstream.id, "upstream missing public_key; skipping");
            continue;
        };
        let narinfo_url = format!("{}/{}.narinfo", base_url.trim_end_matches('/'), path_hash);
        let Ok(resp) = http_client.get(&narinfo_url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.text().await else {
            continue;
        };

        // Only forward narinfos whose Sig matches the upstream's configured
        // trusted public key. Unsigned / wrong-key / tampered narinfos are
        // dropped and we fall through to the next upstream (or 404).
        if !verify_narinfo_signature(public_key, &body) {
            warn!(
                upstream = %upstream.id,
                path_hash,
                "upstream narinfo Sig did not verify against configured public_key; dropping"
            );
            continue;
        }

        // Rewrite the URL: field to proxy through our upstream_nar endpoint.
        let rewritten = body
            .lines()
            .map(|line| {
                if let Some(nar_path) = line.strip_prefix("URL: ") {
                    format!("URL: nar/upstream/{}/{}", upstream.id, nar_path.trim())
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        return Some(rewritten);
    }
    None
}
