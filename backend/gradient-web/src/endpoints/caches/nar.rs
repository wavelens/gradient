/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip, original_text, stored_hash};
use super::refusal::CacheRefusal;
use crate::api_spec::Binary;
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use crate::resource::Resource;
use anyhow::Context;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_core::upstream_source::{
    UpstreamSource, fetch_from_upstream_caches, substitution_sources,
};
use gradient_db::WorkerDb;
use gradient_db::caches::upstream::pull_through_upstream_caches;
use gradient_sources::get_hash_from_url;
use gradient_types::events::cache::NarFetched;
use gradient_types::*;
use gradient_util::shutdown::Shutdown;
use sea_orm::ConnectionTrait;
use std::sync::Arc;
use uuid::Uuid;

#[utoipa::path(
    get,
    path = "/cache/{cache}/nar/{path}",
    operation_id = "getNar",
    tag = "nix-cache",
    summary = "NAR",
    description = "Compressed NAR archive of a store path.",
    params(("cache" = String, Path), ("path" = String, Path)),
    responses((status = OK, description = "NAR stream", body = Binary, content_type = "application/x-nix-nar"), WebError)
)]
pub async fn nar(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
) -> WebResult<Response> {
    let path_hash = get_hash_from_url(path.clone())
        .map_err(|e| CacheRefusal::InvalidHashInUrl(e.to_string()))?;

    if !(path.ends_with(".nar") || path.contains(".nar.")) {
        return Err(WebError::not_found(Resource::Path));
    }

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

    let mapper = state.store_path_mapper(MappingContext::Cache(ctx.cache.id));
    let effective_hash = resolve_effective_hash_db(&state.web_db, &mapper, &path_hash).await?;
    let (size, stream) = super::helpers::fetch_nar_stream(
        &state.web_db,
        &state.nar_storage,
        ctx.cache.id,
        &effective_hash,
    )
    .await?;

    super::super::stats::record_nar_traffic(&state.memory.cache_traffic, ctx.cache.id, size as i64);
    state.events.publish(NarFetched {
        cache: ctx.cache.id,
        hash: original_text(&mapper, effective_hash.clone()),
        size,
    });
    spawn_fetch_stamp(
        &state.shutdown,
        &state.worker_db,
        ctx.cache.id,
        effective_hash,
    );

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-nix-nar"),
        )
        .header(header::CONTENT_LENGTH, size)
        .body(Body::from_stream(stream))
        .context("build response")
        .map_err(Into::into)
}

#[utoipa::path(
    get,
    path = "/cache/{cache}/nar/upstream/{upstream_id}/{path}",
    operation_id = "getUpstreamNar",
    tag = "nix-cache",
    summary = "Upstream NAR",
    description = "Streams a NAR from an upstream of the cache. Any upstream serving the same path is tried when the named one is gone.",
    params(("cache" = String, Path), ("upstream_id" = Uuid, Path), ("path" = String, Path)),
    responses((status = OK, description = "NAR stream", body = Binary, content_type = "application/x-nix-nar"), WebError)
)]
pub async fn upstream_nar(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache_name, upstream_id, path)): Path<(String, Uuid, String)>,
    RawQuery(query): RawQuery,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state.config, &headers, peer);
    let ctx = CacheContext::load(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        &headers,
        client_ip,
        cache_name,
    )
    .await?;

    let upstream_caches = pull_through_upstream_caches(&state.web_db, &ctx.cache).await?;

    let sources = named_first(substitution_sources(&upstream_caches), upstream_id);
    if sources.is_empty() {
        return Err(WebError::not_found(Resource::Upstream));
    }

    let Some(object) =
        fetch_from_upstream_caches(&sources, &upstream_nar_path(&path, query.as_deref()), None)
            .await
    else {
        return Err(WebError::not_found(Resource::NarInUpstream));
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
        .context("build response")
        .map_err(Into::into)
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
    mapper: &StorePathMapper,
    path_hash: &str,
) -> WebResult<String> {
    let candidates = vec![format!("blake3:{path_hash}"), format!("sha256:{path_hash}")];

    let by_cached_path = gradient_db::cached_paths::by_any_file_hash(db, candidates).await?;

    if let Some(row) = by_cached_path {
        return Ok(row.hash);
    }

    stored_hash(mapper, path_hash)
}

fn spawn_fetch_stamp(shutdown: &Shutdown, worker_db: &WorkerDb, cache_id: CacheId, hash: String) {
    let worker_db = worker_db.clone();
    shutdown.spawn(async move {
        let _ = gradient_db::cached_paths::count_fetch(&worker_db, cache_id, &hash).await;
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
    use gradient_fixtures::mock::mock_db;

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

    fn identity() -> StorePathMapper {
        StorePathMapper::new(
            Arc::new(IdentityMapping),
            MappingContext::Cache(CacheId::nil()),
        )
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    #[test]
    fn resolve_returns_store_hash_from_cached_path() {
        let db = mock_db()
            .append_query_results([vec![cached_path_row()]])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, &identity(), FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, STORE_HASH);
    }

    #[test]
    fn an_unknown_file_hash_is_not_found() {
        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();

        let result =
            runtime().block_on(resolve_effective_hash_db(&db, &identity(), FILE_HASH_NIX32));
        assert!(result.is_err());
    }

    #[test]
    fn a_store_hash_in_a_nar_url_is_mapped_inbound() {
        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        let mapper = StorePathMapper::new(
            Arc::new(gradient_entity::RotatedHashMapping),
            MappingContext::Cache(CacheId::nil()),
        );

        let effective = runtime()
            .block_on(resolve_effective_hash_db(
                &db,
                &mapper,
                gradient_entity::EXAMPLE_ORIGINAL_HASH,
            ))
            .expect("resolve should succeed");
        assert_eq!(effective, gradient_entity::EXAMPLE_STORED_HASH);
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

        let db = mock_db()
            .append_query_results([vec![row]])
            .into_connection();

        let effective = runtime()
            .block_on(resolve_effective_hash_db(&db, &identity(), FILE_HASH_NIX32))
            .expect("resolve should succeed");
        assert_eq!(effective, STORE_HASH);
    }
}
