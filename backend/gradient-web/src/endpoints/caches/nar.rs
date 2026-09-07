/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_sources::get_hash_from_url;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

pub async fn nar(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> WebResult<Response> {
    let path_hash =
        get_hash_from_url(path.clone()).map_err(|e| WebError::bad_request(e.to_string()))?;

    if !(path.ends_with(".nar") || path.contains(".nar.")) {
        return Err(WebError::not_found("Path"));
    }

    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;

    let (effective_hash, size, stream) =
        super::helpers::fetch_nar_stream(&state, &path_hash).await?;

    spawn_nar_traffic_metric(Arc::clone(&state), ctx.cache.id, size as i64);
    spawn_cache_derivation_fetch_update(Arc::clone(&state), ctx.cache.id, effective_hash);

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .header(header::CONTENT_LENGTH, size)
        .body(Body::from_stream(stream))
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}

pub async fn upstream_nar(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache_name, upstream_id, path)): Path<(String, Uuid, String)>,
    RawQuery(query): RawQuery,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache_name).await?;

    let upstreams = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(ctx.cache.id))
        .all(&state.web_db)
        .await?;

    let bases = upstream_bases(&upstreams, upstream_id);
    if bases.is_empty() {
        return Err(WebError::not_found("Upstream"));
    }

    let client = gradient_util::http::download_client();
    for base in bases {
        let nar_url = build_upstream_nar_url(&base, &path, query.as_deref());
        let Ok(resp) = client.get(&nar_url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }

        let mut builder = Response::builder().header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        );
        if let Some(len) = resp.content_length() {
            builder = builder.header(header::CONTENT_LENGTH, len);
        }
        return builder
            .body(Body::from_stream(resp.bytes_stream()))
            .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)));
    }

    Err(WebError::not_found("NAR in upstream"))
}

/// Which upstreams to try for a proxied NAR, best first.
///
/// The URL names an upstream by row id, but that row is configuration: removing
/// an upstream would otherwise permanently 404 every narinfo already handed out
/// that names it, because a client caches narinfo and never refetches it. The
/// NAR path itself is content-addressed - the filename is a file hash - so any
/// upstream of this cache that serves that path serves the same bytes. Try the
/// named one first, then the rest, and only give up when none of them has it.
fn upstream_bases(upstreams: &[MCacheUpstream], named: Uuid) -> Vec<String> {
    let mut bases: Vec<String> = Vec::with_capacity(upstreams.len());
    let named_first = upstreams
        .iter()
        .filter(|u| u.id.into_inner() == named)
        .chain(upstreams.iter().filter(|u| u.id.into_inner() != named));

    for upstream in named_first {
        let Some(url) = upstream.url.as_deref() else {
            continue;
        };
        if !bases.iter().any(|b| b == url) {
            bases.push(url.to_owned());
        }
    }
    bases
}

pub(crate) async fn resolve_effective_hash_db<C: ConnectionTrait>(
    db: &C,
    path_hash: &str,
) -> WebResult<String> {
    let candidates = [format!("blake3:{path_hash}"), format!("sha256:{path_hash}")];

    let by_cached_path = ECachedPath::find()
        .filter(CCachedPath::FileHash.is_in(candidates))
        .one(db)
        .await?;

    if let Some(row) = by_cached_path {
        return Ok(row.hash);
    }

    Ok(path_hash.to_string())
}

fn spawn_nar_traffic_metric(state: Arc<ServerState>, cache_id: CacheId, bytes_len: i64) {
    let s = Arc::clone(&state);
    state.shutdown.spawn(async move {
        super::super::stats::record_nar_traffic(s, cache_id, bytes_len).await;
    });
}

/// Bookkeeping update spawned after every successful NAR fetch. Uses
/// `worker_db` (not `web_db`) on purpose: under heavy NAR traffic these
/// fire-and-forget UPDATEs would otherwise contend with foreground HTTP
/// requests on the web pool.
fn spawn_cache_derivation_fetch_update(state: Arc<ServerState>, cache_id: CacheId, hash: String) {
    let s = Arc::clone(&state);
    state.shutdown.spawn(async move {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
        let now = gradient_types::now();
        let now_val = sea_orm::Value::ChronoDateTimeUtc(Some(
            chrono::DateTime::from_naive_utc_and_offset(now, chrono::Utc),
        ));
        let cache_val = sea_orm::Value::Uuid(Some(cache_id.into_inner()));
        let hash_val = sea_orm::Value::String(Some(hash.clone()));

        let _ = s
            .worker_db
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cache_derivation SET last_fetched_at = $1 \
                 WHERE cache = $2 AND derivation IN ( \
                     SELECT derivation FROM derivation_output WHERE hash = $3 AND is_cached = true \
                 )",
                [now_val.clone(), cache_val.clone(), hash_val.clone()],
            ))
            .await;

        let _ = s
            .worker_db
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE cached_path_signature \
                 SET last_fetched_at = $1, fetch_count = fetch_count + 1 \
                 WHERE cache = $2 \
                   AND cached_path = (SELECT id FROM cached_path WHERE hash = $3)",
                [now_val, cache_val, hash_val],
            ))
            .await;
    });
}

/// Reconstruct the upstream NAR URL. The narinfo we re-served kept the upstream's
/// own URL query (e.g. hash-routed caches like `cache.nixos-cuda.org` require
/// `?hash=<storehash>` to resolve the out-hash), but axum's `{*path}` capture
/// drops the query - so a missing forward made the upstream 404 a NAR it has.
fn build_upstream_nar_url(base_url: &str, path: &str, query: Option<&str>) -> String {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    match query {
        Some(q) if !q.is_empty() => format!("{url}?{q}"),
        _ => url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn upstream_row(id: u128, url: Option<&str>) -> MCacheUpstream {
        MCacheUpstream {
            id: gradient_types::ids::CacheUpstreamId::new(uuid::Uuid::from_u128(id)),
            url: url.map(str::to_owned),
            ..Default::default()
        }
    }

    /// The named upstream is still the right first guess: it is the one whose
    /// layout the narinfo was written against.
    #[test]
    fn the_named_upstream_is_tried_first() {
        let rows = vec![
            upstream_row(1, Some("https://a.example")),
            upstream_row(2, Some("https://b.example")),
        ];

        let bases = upstream_bases(&rows, uuid::Uuid::from_u128(2));

        assert_eq!(bases, vec!["https://b.example", "https://a.example"]);
    }

    /// The case that broke a real substitution: the upstream row named in an
    /// already-issued narinfo was deleted. A client caches narinfo and never
    /// refetches, so 404ing here strands that path forever - the remaining
    /// upstreams have to be tried.
    #[test]
    fn a_deleted_upstream_falls_back_to_the_rest() {
        let rows = vec![
            upstream_row(1, Some("https://a.example")),
            upstream_row(2, Some("https://b.example")),
        ];

        let bases = upstream_bases(&rows, uuid::Uuid::from_u128(99));

        assert_eq!(bases, vec!["https://a.example", "https://b.example"]);
    }

    /// An upstream that is another Gradient cache rather than an external URL
    /// has nothing to proxy to; it must be skipped, not turned into an error
    /// that hides the upstreams which would have served the path.
    #[test]
    fn upstreams_without_a_url_are_skipped() {
        let rows = vec![
            upstream_row(1, None),
            upstream_row(2, Some("https://b.example")),
        ];

        assert_eq!(
            upstream_bases(&rows, uuid::Uuid::from_u128(1)),
            vec!["https://b.example"]
        );
        assert!(upstream_bases(&[upstream_row(1, None)], uuid::Uuid::from_u128(1)).is_empty());
    }

    /// The named upstream also appears in the "rest", so without dedup every
    /// fallback would re-fetch it.
    #[test]
    fn the_named_upstream_is_not_tried_twice() {
        let rows = vec![
            upstream_row(1, Some("https://a.example")),
            upstream_row(2, Some("https://a.example")),
        ];

        assert_eq!(
            upstream_bases(&rows, uuid::Uuid::from_u128(1)),
            vec!["https://a.example"]
        );
    }

    // Placeholder file hash (nix32 52-char) as it appears in a narinfo URL.
    const FILE_HASH_NIX32: &str = "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
    const STORE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn cached_path_row() -> gradient_entity::cached_path::Model {
        gradient_entity::cached_path::Model {
            id: gradient_types::ids::CachedPathId::now_v7(),
            hash: STORE_HASH.to_string(),
            package: "hello.drv".to_string(),
            file_hash: Some(format!("sha256:{FILE_HASH_NIX32}")),
            file_size: Some(1234),
            nar_size: Some(2048),
            nar_hash: Some(format!("sha256:{FILE_HASH_NIX32}")),
            created_at: gradient_types::now(),
            ..Default::default()
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    /// `cached_path.file_hash` is the only authoritative source for the URL
    /// → store-hash mapping. The resolver returns the cached_path's store
    /// hash, which is the key the NAR blob was written under.
    #[test]
    fn resolve_returns_store_hash_from_cached_path() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![cached_path_row()]])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, STORE_HASH);
    }

    /// When no cached_path matches, the URL hash is returned unchanged
    /// (legacy/direct-hash URL behaviour preserved).
    #[test]
    fn resolve_falls_back_to_url_hash_when_no_match() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, FILE_HASH_NIX32);
    }

    /// A hash-routed upstream (e.g. `cache.nixos-cuda.org`) needs the
    /// `?hash=<storehash>` query the re-served narinfo carried; dropping it 404s a
    /// NAR the upstream has.
    #[test]
    fn upstream_nar_url_forwards_query_string() {
        assert_eq!(
            build_upstream_nar_url(
                "https://cache.nixos-cuda.org",
                "nar/0njia.nar",
                Some("hash=6713ipl")
            ),
            "https://cache.nixos-cuda.org/nar/0njia.nar?hash=6713ipl"
        );
        assert_eq!(
            build_upstream_nar_url("https://up/", "nar/x.nar", None),
            "https://up/nar/x.nar"
        );
        assert_eq!(
            build_upstream_nar_url("https://up", "nar/x.nar", Some("")),
            "https://up/nar/x.nar"
        );
    }

    /// Rows uploaded while issue #132's BLAKE3 default was active carry
    /// `blake3:`-prefixed file hashes. The URL slug carries the bare nix32
    /// digest with no algorithm prefix, so the resolver must look up both
    /// `blake3:` and `sha256:` to bridge the two generations.
    #[test]
    fn resolve_returns_store_hash_for_blake3_file_hash() {
        let mut row = cached_path_row();
        row.file_hash = Some(format!("blake3:{FILE_HASH_NIX32}"));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, STORE_HASH);
    }
}
