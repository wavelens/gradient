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
use gradient_core::upstream_source::{
    UpstreamSource, fetch_from_upstream_caches, substitution_sources,
};
use gradient_db::caches::upstream::active_upstream_caches;
use gradient_sources::get_hash_from_url;
use gradient_types::events::cache::NarFetched;
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
        super::helpers::fetch_nar_stream(&state, ctx.cache.id, &path_hash).await?;

    super::super::stats::record_nar_traffic(&state, ctx.cache.id, size as i64);
    state.events.publish(NarFetched {
        cache: ctx.cache.id,
        hash: effective_hash.clone(),
        size,
    });
    spawn_fetch_stamp(Arc::clone(&state), ctx.cache.id, effective_hash);

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

    let upstream_caches = active_upstream_caches(&state.web_db, ctx.cache.id).await?;

    let sources = named_first(substitution_sources(&upstream_caches), upstream_id);
    if sources.is_empty() {
        return Err(WebError::not_found("Upstream"));
    }

    let Some(object) =
        fetch_from_upstream_caches(&sources, &upstream_nar_path(&path, query.as_deref()), None)
            .await
    else {
        return Err(WebError::not_found("NAR in upstream"));
    };

    let mut builder = Response::builder().header(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-nix-nar"),
    );
    if let Some(len) = object.content_length {
        builder = builder.header(header::CONTENT_LENGTH, len);
    }
    builder
        .body(Body::from_stream(object.body))
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}

/// A client is caching a narinfo and never refetching it. Removing an upstream row must not 404
/// every narinfo naming it. The NAR file is content-addressed. Any upstream of this cache serving
/// the path is serving the same bytes.
fn named_first(sources: Vec<UpstreamSource>, named: Uuid) -> Vec<UpstreamSource> {
    let (mut ordered, rest): (Vec<_>, Vec<_>) = sources
        .into_iter()
        .partition(|s| s.id.into_inner() == named);
    for source in rest {
        if !ordered.iter().any(|o| o.url == source.url) {
            ordered.push(source);
        }
    }
    ordered
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

gradient_db::sql! {
    TOUCH_CACHED_PATH_SIGNATURE_FETCH = "UPDATE cached_path_signature \
         SET last_fetched_at = $1, fetch_count = fetch_count + 1 \
         WHERE cache = $2 \
           AND cached_path = (SELECT id FROM cached_path WHERE hash = $3)",
        params = [Now, CacheId, CachedPathHash];
}

fn spawn_fetch_stamp(state: Arc<ServerState>, cache_id: CacheId, hash: String) {
    let s = Arc::clone(&state);
    state.shutdown.spawn(async move {
        use sea_orm::ConnectionTrait;
        let now = gradient_types::now();
        let now_val = sea_orm::Value::ChronoDateTimeUtc(Some(
            chrono::DateTime::from_naive_utc_and_offset(now, chrono::Utc),
        ));
        let cache_val = sea_orm::Value::Uuid(Some(cache_id.into_inner()));
        let hash_val = sea_orm::Value::String(Some(hash.clone()));

        let _ = s
            .worker_db
            .execute_raw(TOUCH_CACHED_PATH_SIGNATURE_FETCH.bind([now_val, cache_val, hash_val]))
            .await;
    });
}

/// axum's `{*path}` capture is dropping the query. Hash-routed upstreams like
/// `cache.nixos-cuda.org` need `?hash=<storehash>` and answer 404 without it.
fn upstream_nar_path(path: &str, query: Option<&str>) -> String {
    match query {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path.to_owned(),
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

    fn upstream_bases(rows: &[MCacheUpstream], named: Uuid) -> Vec<String> {
        named_first(substitution_sources(rows), named)
            .into_iter()
            .map(|s| s.url)
            .collect()
    }

    #[test]
    fn a_write_only_upstream_is_never_proxied() {
        let rows = vec![
            MCacheUpstream {
                mode: gradient_entity::project_cache::CacheSubscriptionMode::WriteOnly,
                ..upstream_row(1, Some("https://push.example"))
            },
            upstream_row(2, Some("https://b.example")),
        ];

        assert_eq!(
            upstream_bases(&rows, uuid::Uuid::from_u128(1)),
            vec!["https://b.example"]
        );
    }

    #[test]
    fn the_named_upstream_is_tried_first() {
        let rows = vec![
            upstream_row(1, Some("https://a.example")),
            upstream_row(2, Some("https://b.example")),
        ];

        let bases = upstream_bases(&rows, uuid::Uuid::from_u128(2));

        assert_eq!(bases, vec!["https://b.example", "https://a.example"]);
    }

    #[test]
    fn a_deleted_upstream_falls_back_to_the_rest() {
        let rows = vec![
            upstream_row(1, Some("https://a.example")),
            upstream_row(2, Some("https://b.example")),
        ];

        let bases = upstream_bases(&rows, uuid::Uuid::from_u128(99));

        assert_eq!(bases, vec!["https://a.example", "https://b.example"]);
    }

    #[test]
    fn upstream_caches_without_a_url_are_skipped() {
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

    #[test]
    fn upstream_nar_path_forwards_query_string() {
        assert_eq!(
            upstream_nar_path("nar/0njia.nar", Some("hash=6713ipl")),
            "nar/0njia.nar?hash=6713ipl"
        );
        assert_eq!(upstream_nar_path("nar/x.nar", None), "nar/x.nar");
        assert_eq!(upstream_nar_path("nar/x.nar", Some("")), "nar/x.nar");
    }

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
