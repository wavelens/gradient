/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! An unknown build id must be a `404`, never another error status. debuginfod clients are
//! abandoning the whole lookup on anything else.

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_core::upstream_source::UpstreamSource;
use gradient_db::caches::upstream::active_upstream_caches;
use gradient_types::ids::{CacheId, CacheUpstreamId};
use gradient_util::nix_hash::{normalize_nar_hash, strip_hash_algo};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
        gradient_db::caches::debug_info::lookup_for_cache(&state.web_db, ctx.cache.id, &build_id)
            .await?
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

    let upstream_caches = upstream_caches_for(&state, ctx.cache.id).await;
    match fetch_from_upstream_caches(&upstream_caches, &build_id).await {
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

async fn upstream_caches_for(state: &Arc<ServerState>, cache: CacheId) -> Vec<UpstreamSource> {
    active_upstream_caches(&state.web_db, cache)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|upstream| {
            Some(UpstreamSource {
                id: upstream.id,
                url: upstream.url?,
                http1_only: upstream.http1_only,
            })
        })
        .collect()
}

async fn fetch_from_upstream_caches(
    upstream_caches: &[UpstreamSource],
    build_id: &str,
) -> Option<DebugInfoRedirect> {
    let pins = gradient_core::upstream::http1_pins();
    for upstream in upstream_caches {
        for key in [build_id.to_owned(), format!("{build_id}.debug")] {
            let url = format!("{}/debuginfo/{}", upstream.url.trim_end_matches('/'), key);
            let Ok(response) = gradient_util::http1_fallback::get(
                &url,
                pins.wants_http1(upstream.id, upstream.http1_only),
                pins.on_failure(upstream.id),
                |r| r,
            )
            .await
            else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(doc) = response.json::<DebugInfoRedirect>().await else {
                continue;
            };
            if let Some(archive) = proxied_archive(upstream.id, &doc.archive) {
                return Some(DebugInfoRedirect {
                    archive,
                    member: doc.member,
                });
            }
        }
    }

    None
}

/// The upstream link is relative to its own `debuginfo/` key. One leading `..` is walking to the
/// upstream root. Anything absolute or reaching past that root is refused.
fn proxied_archive(upstream_id: CacheUpstreamId, archive: &str) -> Option<String> {
    let rest = archive.strip_prefix("../")?;
    if rest.is_empty() || rest.starts_with('/') || rest.split('/').any(|seg| seg == "..") {
        return None;
    }

    Some(format!("../nar/upstream/{upstream_id}/{rest}"))
}

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
