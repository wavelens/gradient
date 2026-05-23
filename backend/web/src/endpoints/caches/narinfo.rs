/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, JsonFlag, get_nar_by_hash};
use crate::error::{WebError, WebResult};
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use gradient_core::sources::{get_hash_from_url, verify_narinfo_signature};
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::warn;

/// Optional peer address - mirrors `ConnectInfo`'s real / `MockConnectInfo`
/// fallback, but yields `None` instead of 500 when the runtime wired neither
/// (`axum_test::TestServer` without `MockConnectInfo`, ad-hoc tower stacks).
pub struct OptionalPeer(pub Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for OptionalPeer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let addr = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .or_else(|| {
                parts
                    .extensions
                    .get::<MockConnectInfo<SocketAddr>>()
                    .map(|m| m.0)
            });
        Ok(OptionalPeer(addr))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn text_response(content_type: &'static str, body: String) -> WebResult<Response<String>> {
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(body)
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path(cache): Path<String>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    let ctx = CacheContext::load(&state, &headers, cache).await?;

    let priority = match (ctx.cache.local_priority, peer) {
        (Some(p), Some(addr)) if p != 0 => {
            let client_ip = crate::client_ip::resolve_client_ip(
                &headers,
                addr.ip(),
                &state.config.network.trusted_proxies,
            );
            if in_any(client_ip, &state.config.network.local_ips) {
                p
            } else {
                ctx.cache.priority
            }
        }
        _ => ctx.cache.priority,
    };

    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority,
    };

    if flag.is_set() {
        Ok(axum::Json(res).into_response())
    } else {
        Ok(text_response("text/x-nix-cache-info", res.to_nix_string())?.into_response())
    }
}

pub async fn gradient_cache_info(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(cache): Path<String>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    CacheContext::load(&state, &headers, cache).await?;

    let info = GradientCacheInfo {
        gradient_version: env!("CARGO_PKG_VERSION").to_string(),
        gradient_url: state.config.server.serve_url.clone(),
    };

    if flag.is_set() {
        Ok(axum::Json(info).into_response())
    } else {
        Ok(text_response("text/x-gradient-cache-info", info.to_nix_string())?.into_response())
    }
}

pub async fn path(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    let path_hash =
        get_hash_from_url(path.clone()).map_err(|e| WebError::bad_request(e.to_string()))?;

    if !path.ends_with(".narinfo") {
        return Err(WebError::not_found("Path"));
    }

    let ctx = CacheContext::load(&state, &headers, cache).await?;

    if let Ok(path_info) =
        get_nar_by_hash(Arc::clone(&state), ctx.cache.clone(), path_hash.clone()).await
    {
        if flag.is_set() {
            return Ok(axum::Json(path_info).into_response());
        }
        return Ok(text_response("text/x-nix-narinfo", path_info.to_nix_string())?.into_response());
    }

    let rewritten = fetch_from_upstream(&state, &ctx.cache, &path_hash).await;
    if let Some(body) = rewritten {
        if flag.is_set() {
            return match gradient_core::types::parse_narinfo_body(&body) {
                Ok(parsed) => Ok(axum::Json(parsed).into_response()),
                Err(_) => Err(WebError::internal("Upstream narinfo malformed")),
            };
        }
        return Ok(text_response("text/x-nix-narinfo", body)?.into_response());
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
        .all(&state.web_db)
        .await
        .unwrap_or_default();

    let http_client = &state.http;
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
