/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /cache/{cache}/debuginfo/{build_id}` - the debuginfod entry point of a
//! Nix binary cache.
//!
//! Answers exactly what nix writes under `index-debug-info=true`: a small JSON
//! document naming the NAR that carries the debug file and the member inside it.
//! `archive` is relative to the requested key, so `../nar/<file>.nar.zst`
//! resolves against the cache root the same way it does on cache.nixos.org.
//! Clients probe both `<build-id>` (hydra) and `<build-id>.debug` (file caches
//! written by `nix copy`); both spellings resolve here.
//!
//! A path this cache substituted rather than built has its debug info upstream,
//! so a miss falls through to the cache's upstreams and rewrites their `archive`
//! link through our NAR proxy - the same pull-through behaviour `/log` has. An
//! unknown build id is a `404`, never another error status: debuginfod clients
//! abandon the whole lookup on anything else.

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_types::ids::{CacheId, CacheUpstreamId};
use gradient_types::*;
use gradient_util::nix_hash::{normalize_nar_hash, strip_hash_algo};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Body of a `debuginfo/{build_id}` document, byte-compatible with nix's own.
#[derive(Debug, Deserialize, Serialize)]
struct DebugInfoRedirect {
    archive: String,
    member: String,
}

pub async fn debuginfo(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, build_id)): Path<(String, String)>,
) -> WebResult<Response> {
    let build_id = parse_build_id(&build_id).ok_or_else(|| WebError::not_found("DebugInfo"))?;

    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;

    if let Some(target) =
        gradient_db::lookup_for_cache(&state.web_db, ctx.cache.id, &build_id).await?
    {
        let file_hash = strip_hash_algo(&normalize_nar_hash(&target.file_hash)).to_string();
        return Ok(redirect_response(
            DebugInfoRedirect {
                archive: format!("../nar/{file_hash}.nar.zst"),
                member: target.member,
            },
            "HIT",
        ));
    }

    let upstreams = upstreams_for(&state, ctx.cache.id).await;
    match fetch_from_upstreams(&state.http, &upstreams, &build_id).await {
        Some(doc) => Ok(redirect_response(doc, "MISS")),
        None => Err(WebError::not_found("DebugInfo")),
    }
}

fn redirect_response(doc: DebugInfoRedirect, cache_status: &'static str) -> Response {
    let mut response = axum::Json(doc).into_response();
    let headers = response.headers_mut();
    headers.insert("x-cache", HeaderValue::from_static(cache_status));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

async fn upstreams_for(state: &Arc<ServerState>, cache: CacheId) -> Vec<(CacheUpstreamId, String)> {
    ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|upstream| Some((upstream.id, upstream.url?)))
        .collect()
}

/// Ask each upstream in turn for both spellings of the key and return the first
/// well-formed document, with its `archive` link pointed back at our NAR proxy.
///
/// Unlike a narinfo there is nothing to verify - nix signs store paths, not the
/// debug index - so an upstream that 404s, errors, or answers something we
/// cannot rewrite is skipped.
async fn fetch_from_upstreams(
    client: &reqwest::Client,
    upstreams: &[(CacheUpstreamId, String)],
    build_id: &str,
) -> Option<DebugInfoRedirect> {
    for (upstream_id, base_url) in upstreams {
        for key in [build_id.to_owned(), format!("{build_id}.debug")] {
            let url = format!("{}/debuginfo/{}", base_url.trim_end_matches('/'), key);
            let Ok(response) = client.get(&url).send().await else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(doc) = response.json::<DebugInfoRedirect>().await else {
                continue;
            };
            if let Some(archive) = proxied_archive(*upstream_id, &doc.archive) {
                return Some(DebugInfoRedirect {
                    archive,
                    member: doc.member,
                });
            }
        }
    }

    None
}

/// Rewrites an upstream's `archive` link to route through
/// `/cache/{cache}/nar/upstream/{id}/{path}`. The upstream link is relative to
/// its own `debuginfo/` key, so one leading `..` walks to the upstream root;
/// anything absolute or reaching past that root is refused rather than proxied.
fn proxied_archive(upstream_id: CacheUpstreamId, archive: &str) -> Option<String> {
    let rest = archive.strip_prefix("../")?;
    if rest.is_empty() || rest.starts_with('/') || rest.split('/').any(|seg| seg == "..") {
        return None;
    }

    Some(format!("../nar/upstream/{upstream_id}/{rest}"))
}

/// Accepts `<40 hex>` and `<40 hex>.debug`, the two spellings nix produces.
fn parse_build_id(raw: &str) -> Option<String> {
    let id = raw.strip_suffix(".debug").unwrap_or(raw);
    let ok = id.len() == 40 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    ok.then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{parse_build_id, proxied_archive};
    use gradient_types::ids::CacheUpstreamId;

    const BUILD_ID: &str = "7dbeaca53fbc9a489b633871093c37dae3857a37";

    fn upstream() -> CacheUpstreamId {
        CacheUpstreamId::new(uuid::Uuid::nil())
    }

    #[test]
    fn both_nix_spellings_resolve_to_the_same_build_id() {
        assert_eq!(parse_build_id(BUILD_ID).as_deref(), Some(BUILD_ID));
        assert_eq!(
            parse_build_id(&format!("{BUILD_ID}.debug")).as_deref(),
            Some(BUILD_ID)
        );
    }

    #[test]
    fn anything_that_is_not_a_build_id_is_rejected() {
        assert_eq!(parse_build_id(""), None);
        assert_eq!(parse_build_id("nix-cache-info"), None);
        assert_eq!(parse_build_id(&BUILD_ID[..39]), None);
        assert_eq!(parse_build_id(&BUILD_ID.to_uppercase()), None);
        assert_eq!(parse_build_id(&format!("{BUILD_ID}.narinfo")), None);
    }

    #[test]
    fn an_upstream_archive_is_routed_through_the_nar_proxy() {
        assert_eq!(
            proxied_archive(upstream(), "../nar/abc.nar.xz").as_deref(),
            Some("../nar/upstream/00000000-0000-0000-0000-000000000000/nar/abc.nar.xz")
        );
    }

    #[test]
    fn an_archive_that_escapes_the_upstream_root_is_refused() {
        assert_eq!(proxied_archive(upstream(), "nar/abc.nar.xz"), None);
        assert_eq!(proxied_archive(upstream(), "/nar/abc.nar.xz"), None);
        assert_eq!(proxied_archive(upstream(), "../../etc/passwd"), None);
        assert_eq!(
            proxied_archive(upstream(), "https://evil.example/nar.xz"),
            None
        );
        assert_eq!(proxied_archive(upstream(), "../"), None);
    }
}
