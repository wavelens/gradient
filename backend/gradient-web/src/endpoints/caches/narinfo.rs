/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, JsonFlag, cache_client_ip, get_nar_by_hash};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use crate::extractors::{Path, Query};
use crate::resource::Resource;
use anyhow::Context;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_core::upstream::UpstreamProbe;
use gradient_core::upstream_source::{credential_of_upstream_row, substitutes_from};
use gradient_db::WebDb;
use gradient_db::caches::upstream::pull_through_upstream_caches;
use gradient_sources::{CacheSigner, get_hash_from_url};
use gradient_types::events::cache::NarinfoServed;
use gradient_types::*;
use std::sync::Arc;
use tracing::warn;

fn text_response(content_type: &'static str, body: String) -> WebResult<Response<String>> {
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(body)
        .context("build response")
        .map_err(Into::into)
}

fn with_narinfo_headers(mut response: Response, cache_status: &'static str) -> Response {
    let headers = response.headers_mut();
    headers.insert("x-cache", HeaderValue::from_static(cache_status));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

#[utoipa::path(
    get,
    path = "/cache/{cache}",
    operation_id = "getCacheRoot",
    tag = "nix-cache",
    summary = "Cache root",
    description = "The response is a redirect to `nix-cache-info`. A request for an HTML page goes to the cache page of the web frontend instead.",
    params(("cache" = String, Path)),
    responses((status = FOUND, description = "Redirect to `/cache/{cache}/nix-cache-info`"))
)]
pub async fn cache_root(Path(cache): Path<String>) -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, format!("/cache/{cache}/nix-cache-info"))],
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/cache/{cache}/nix-cache-info",
    operation_id = "getNixCacheInfo",
    tag = "nix-cache",
    summary = "Nix cache info",
    description = "Cache metadata read by Nix substituters. `Priority` is the cache priority, or the local priority for clients in `GRADIENT_HTTP_LOCAL_IPS`.",
    params(("cache" = String, Path), JsonFlag),
    responses((status = OK, description = "Cache metadata", content((String = "text/x-nix-cache-info"), (NixCacheInfo = "application/json"))), WebError)
)]
pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path(cache): Path<String>,
    Query(flag): Query<JsonFlag>,
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

#[utoipa::path(
    get,
    path = "/cache/{cache}/gradient-cache-info",
    operation_id = "getGradientCacheInfo",
    tag = "nix-cache",
    summary = "Gradient cache info",
    description = "Gradient version and server URL behind this cache, for clients probing a substituter. Sends `Access-Control-Allow-Origin: *`.",
    params(("cache" = String, Path), JsonFlag),
    responses((status = OK, description = "Gradient metadata", content((String = "text/x-gradient-cache-info"), (GradientCacheInfo = "application/json"))), WebError)
)]
pub async fn gradient_cache_info(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path(cache): Path<String>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state.config, &headers, peer);
    CacheContext::load(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        &headers,
        client_ip,
        cache,
    )
    .await?;

    let info = GradientCacheInfo {
        gradient_version: env!("CARGO_PKG_VERSION").to_string(),
        gradient_url: state.config.server.serve_url.clone(),
    };

    let mut response = if flag.is_set() {
        axum::Json(info).into_response()
    } else {
        text_response("text/x-gradient-cache-info", info.to_nix_string())?.into_response()
    };
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/cache/{cache}/{path}",
    operation_id = "getNarinfo",
    tag = "nix-cache",
    summary = "Narinfo",
    description = "Path info of a store path. A miss here is looked up in the cache's upstream caches, re-signed with the cache key. `X-Cache` reports `HIT` (ours) or `MISS` (upstream).",
    params(("cache" = String, Path), ("path" = String, Path), JsonFlag),
    responses((status = OK, description = "Narinfo", content((String = "text/x-nix-narinfo"), (NixPathInfo = "application/json"))), WebError)
)]
pub async fn path(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    // A 4xx other than 404 is read as a hard error by Nix clients and debuginfod.
    // nixseparatedebuginfod is aborting the whole request instead of trying the next substituter
    // (#563).
    if !path.ends_with(".narinfo") {
        return Err(WebError::not_found(Resource::Path));
    }

    let path_hash =
        get_hash_from_url(path.clone()).map_err(|_| WebError::not_found(Resource::Path))?;
    let store_hash =
        StoreHash::parse(&path_hash).map_err(|_| WebError::not_found(Resource::Path))?;

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
    let stored_hash = mapper.inbound_hash(store_hash.clone()).to_string();
    if let Ok(path_info) = get_nar_by_hash(
        &state.web_db,
        &state.config,
        ctx.cache.clone(),
        &mapper,
        store_hash,
    )
    .await
    {
        state.events.publish(NarinfoServed {
            cache: ctx.cache.id,
            hash: path_hash.clone(),
            hit: true,
        });
        let response =
            build_narinfo_http_response(path_info, flag.is_set(), &stored_hash, ctx.cache.id)?;
        return Ok(with_narinfo_headers(response, "HIT"));
    }

    let proxied = fetch_from_upstream(
        &state.web_db,
        &state.user_supplied_url_http_client,
        &state.config,
        &ctx.cache,
        &path_hash,
    )
    .await;
    if let Some(path_info) = proxied {
        state.events.publish(NarinfoServed {
            cache: ctx.cache.id,
            hash: path_hash.clone(),
            hit: false,
        });
        let response =
            build_narinfo_http_response(path_info, flag.is_set(), &path_hash, ctx.cache.id)?;
        return Ok(with_narinfo_headers(response, "MISS"));
    }

    Err(WebError::not_found(Resource::Path))
}

fn build_narinfo_http_response(
    path_info: NixPathInfo,
    json: bool,
    stored_hash: &str,
    cache_id: CacheId,
) -> WebResult<Response> {
    Ok(if json {
        axum::Json(path_info).into_response()
    } else {
        text_response(
            "text/x-nix-narinfo",
            path_info.to_nix_string().with_context(|| {
                format!(
                    "render narinfo for the store path with hash {stored_hash} in cache {cache_id}"
                )
            })?,
        )?
        .into_response()
    })
}

async fn fetch_from_upstream(
    db: &WebDb,
    http: &gradient_util::http::HttpClientForUserSuppliedUrls,
    config: &RuntimeConfig,
    cache: &MCache,
    path_hash: &str,
) -> Option<NixPathInfo> {
    let mut upstream_caches: Vec<UpstreamProbe> = Vec::new();
    for upstream in pull_through_upstream_caches(db, cache)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(substitutes_from)
    {
        let Some(url) = upstream.url.as_deref().and_then(|stored| {
            gradient_db::caches::upstream::parse_stored_upstream_cache_url(upstream.id, stored)
        }) else {
            continue;
        };
        let Ok(credential) =
            credential_of_upstream_row(&config.secrets.crypt_file, &upstream).await
        else {
            warn!(upstream = %upstream.id, "can't decrypt credentials for the upstream cache. skipping this upstream.");
            continue;
        };
        let Some(public_key) = upstream.public_key else {
            warn!(upstream = %upstream.id, "upstream missing public_key; skipping");
            continue;
        };
        upstream_caches.push(UpstreamProbe {
            id: upstream.id,
            url,
            public_key,
            http1_only: upstream.http1_only,
            credential,
        });
    }

    let found = gradient_core::upstream::find_first_signed_upstream_narinfo(
        http,
        &upstream_caches,
        path_hash,
    )
    .await?;

    let mut path_info = found.path_info;
    path_info.url = format!("nar/upstream/{}/{}", found.upstream, path_info.url);

    match CacheSigner::from_cache(&config.secrets.crypt_file, cache, &config.server.serve_url) {
        Ok(signer) => {
            let signature = signer.sign_parsed_narinfo(&path_info);
            path_info.signature_lines.push(signature);
        }
        Err(e) => {
            warn!(cache = %cache.name, error = %e, "cannot re-sign a proxied narinfo; serving it with the upstream's signature only");
        }
    }
    Some(path_info)
}
