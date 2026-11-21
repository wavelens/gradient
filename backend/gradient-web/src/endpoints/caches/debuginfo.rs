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
use crate::extractors::Path;
use crate::resource::Resource;
use crate::store_paths::original_text;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_core::upstream_source::{UpstreamSource, credential_of_upstream_row};
use gradient_db::WebDb;
use gradient_db::caches::debug_info::DebugInfoTarget;
use gradient_db::caches::upstream::pull_through_upstream_caches;
use gradient_types::ids::CacheUpstreamId;
use gradient_types::{MCache, StorePathMapper};
use gradient_util::http::{HttpProtocolSelection, read_response_body_with_limit};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const MAX_UPSTREAM_DEBUGINFO_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
struct DebugInfoRedirect {
    archive: String,
    member: String,
}

impl DebugInfoRedirect {
    fn for_cached_path(target: DebugInfoTarget, mapper: &StorePathMapper) -> Self {
        Self {
            archive: format!("../nar/{}.nar.zst", original_text(mapper, target.hash)),
            member: target.member,
        }
    }
}

#[utoipa::path(
    get,
    path = "/cache/{cache}/debuginfo/{build_id}",
    operation_id = "getDebugInfo",
    tag = "nix-cache",
    summary = "Debug info",
    description = "DWARF debug info for an ELF build id, in the Nix `index-debug-info` format. Indexed from `separateDebugInfo` outputs; a miss falls through to the upstream caches. `X-Cache` reports `HIT` or `MISS`.",
    params(("cache" = String, Path), ("build_id" = String, Path)),
    responses((status = OK, description = "Debug info redirect", body = DebugInfoRedirect), WebError)
)]
pub async fn debuginfo(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, build_id)): Path<(String, String)>,
) -> WebResult<Response> {
    let build_id =
        parse_build_id(&build_id).ok_or_else(|| WebError::not_found(Resource::DebugInfo))?;

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

    if let Some(target) =
        gradient_db::caches::debug_info::lookup_for_cache(&state.web_db, ctx.cache.id, &build_id)
            .await?
    {
        let mapper = state.cache_mapper(ctx.cache.id);
        return Ok(redirect_response(
            DebugInfoRedirect::for_cached_path(target, &mapper),
            "HIT",
        ));
    }

    let upstream_caches =
        upstream_caches_for(&state.web_db, &ctx.cache, &state.config.secrets.crypt_file).await;
    match fetch_from_upstream_caches(
        &state.user_supplied_url_http_client,
        &upstream_caches,
        &build_id,
    )
    .await
    {
        Some(doc) => Ok(redirect_response(doc, "MISS")),
        None => Err(WebError::not_found(Resource::DebugInfo)),
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

async fn upstream_caches_for(db: &WebDb, cache: &MCache, crypt_file: &str) -> Vec<UpstreamSource> {
    let mut sources = Vec::new();
    for upstream in pull_through_upstream_caches(db, cache)
        .await
        .unwrap_or_default()
    {
        let Some(url) = upstream.url.as_deref().and_then(|stored| {
            gradient_db::caches::upstream::parse_stored_upstream_cache_url(upstream.id, stored)
        }) else {
            continue;
        };
        let Ok(credential) = credential_of_upstream_row(crypt_file, &upstream).await else {
            tracing::warn!(upstream = %upstream.id, "can't decrypt credentials for the upstream cache. skipping this upstream.");
            continue;
        };
        sources.push(UpstreamSource {
            id: upstream.id,
            url,
            http1_only: upstream.http1_only,
            credential,
        });
    }
    sources
}

async fn fetch_from_upstream_caches(
    http: &gradient_util::http::HttpClientForUserSuppliedUrls,
    upstream_caches: &[UpstreamSource],
    build_id: &str,
) -> Option<DebugInfoRedirect> {
    for upstream in upstream_caches {
        for key in [build_id.to_owned(), format!("{build_id}.debug")] {
            let url = upstream
                .url
                .append_api_path_to_base_url(&format!("debuginfo/{key}"));
            let protocol =
                HttpProtocolSelection::select_http_protocol_from_cache_row(upstream.http1_only);
            let Ok(request) =
                http.download_get_request_builder(&url, protocol, upstream.credential.as_deref())
            else {
                continue;
            };
            let Ok(response) = request.send().await else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(body) =
                read_response_body_with_limit(response, MAX_UPSTREAM_DEBUGINFO_RESPONSE_BYTES)
                    .await
            else {
                continue;
            };
            let Ok(mut doc) = serde_json::from_slice::<DebugInfoRedirect>(&body) else {
                continue;
            };
            if let Some(archive) = proxied_archive(upstream.id, &doc.archive) {
                doc.archive = archive;
                return Some(doc);
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
    use super::{UpstreamSource, fetch_from_upstream_caches, parse_build_id, proxied_archive};
    use gradient_types::ids::CacheUpstreamId;
    use gradient_util::http::HttpClientForUserSuppliedUrls;
    use gradient_util::http_validation::UserUrlDestinationAllowlist;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const BUILD_ID: &str = "7dbeaca53fbc9a489b633871093c37dae3857a37";

    fn upstream() -> CacheUpstreamId {
        CacheUpstreamId::new(uuid::Uuid::nil())
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "test upstream cache server with endless response body"
    )]
    async fn spawn_upstream_with_endless_json_body() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let _ = stream.read(&mut request).await;
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n";
                    if stream.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let mut chunk = format!("{:x}\r\n", 8192 + 12).into_bytes();
                    chunk.extend_from_slice(b"{\"archive\":\"");
                    chunk.extend(std::iter::repeat_n(b'a', 8192));
                    chunk.extend_from_slice(b"\r\n");
                    let filler = [b"2000\r\n".as_slice(), &[b'a'; 8192], b"\r\n"].concat();
                    if stream.write_all(&chunk).await.is_err() {
                        return;
                    }
                    while stream.write_all(&filler).await.is_ok() {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                });
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn an_endless_debuginfo_body_from_an_upstream_cache_is_cut_off() {
        let base = spawn_upstream_with_endless_json_body().await;
        let http = HttpClientForUserSuppliedUrls::new(
            UserUrlDestinationAllowlist::parse(&["127.0.0.0/8".to_owned()]).expect("allowlist"),
        )
        .expect("client");
        let upstream_caches = [UpstreamSource {
            id: upstream(),
            url: base.parse().expect("base url"),
            http1_only: true,
            credential: None,
        }];
        let fetched = tokio::time::timeout(
            Duration::from_secs(5),
            fetch_from_upstream_caches(&http, &upstream_caches, BUILD_ID),
        )
        .await
        .expect("the fetch stops reading after the size limit");
        assert!(fetched.is_none());
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
