/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::delete_nar_from_cache;
use super::refusal::CacheRefusal;
use crate::api_spec::CachePath;
use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::resource::Resource;
use crate::scope::{CacheScope, Readable, Requires, can};
use anyhow::Context;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::cached_paths::{CacheTotals, NarFilter, NarOrder, NarSummary};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// axum's urlencoded query decoder cannot deserialize integer fields through a flattened struct.
// Pagination is a dedicated `Query<PaginationParams>` extractor for this reason.
#[derive(Debug, Deserialize, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NarListQuery {
    pub hash: Option<String>,
    pub package: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NarDetail {
    pub hash: String,
    pub store_path: String,
    pub package: String,
    pub nar_size: Option<i64>,
    pub file_size: Option<i64>,
    pub file_hash: Option<String>,
    pub nar_hash: Option<String>,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub created_at: NaiveDateTime,
    pub last_fetched_at: Option<NaiveDateTime>,
    pub fetch_count: i64,
    pub signed: bool,
}

impl NarDetail {
    fn new(path: MCachedPath, signature: MCachedPathSignature, references: Vec<String>) -> Self {
        Self {
            store_path: path.as_store_path().base(),
            hash: path.hash,
            package: path.package,
            nar_size: path.nar_size,
            file_size: path.file_size,
            file_hash: path.file_hash,
            nar_hash: path.nar_hash,
            references,
            deriver: path.deriver,
            ca: path.ca,
            created_at: path.created_at,
            last_fetched_at: signature.last_fetched_at,
            fetch_count: signature.fetch_count,
            signed: signature.signature.is_some(),
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NarStats {
    pub total_nars: i64,
    pub total_nar_size: i64,
    pub total_file_size: i64,
    pub last_uploaded_at: Option<NaiveDateTime>,
    pub oldest_fetched_at: Option<NaiveDateTime>,
}

impl From<CacheTotals> for NarStats {
    fn from(totals: CacheTotals) -> Self {
        Self {
            total_nars: totals.total_nars,
            total_nar_size: totals.total_nar_size.unwrap_or(0),
            total_file_size: totals.total_file_size.unwrap_or(0),
            last_uploaded_at: totals.last_uploaded_at,
            oldest_fetched_at: totals.oldest_fetched_at,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NarAvailable {
    pub available: bool,
}

const DEFAULT_PER_PAGE: u64 = 50;
const MAX_PER_PAGE: u64 = 200;

type PaginatedNarSummary = Paginated<NarSummary>;

#[utoipa::path(
    get,
    path = "/caches/{cache}/nars",
    operation_id = "listCacheNars",
    tag = "caches",
    summary = "List cached NARs",
    description = "Returns a paginated list of NARs (cached store paths) currently signed by this cache.\nPublic caches allow anonymous access; otherwise requires `readStore`.",
    params(CachePath, NarListQuery, PaginationParams),
    responses((status = OK, description = "Paginated NAR list", body = PaginatedNarSummary), WebError)
)]
pub async fn list(
    CacheScope { cache, db, .. }: CacheScope<Readable>,
    Query(q): Query<NarListQuery>,
    Query(pagination): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<NarSummary>>>> {
    let per_page = pagination
        .per_page
        .unwrap_or(DEFAULT_PER_PAGE)
        .clamp(1, MAX_PER_PAGE);
    let page = pagination.page.unwrap_or(1).max(1);
    let offset = (page - 1) * per_page;

    let ascending = matches!(q.order.as_deref(), Some("asc"));
    let order = match q.sort.as_deref().unwrap_or("created_at") {
        "nar_size" => NarOrder::NarSize,
        "last_fetched_at" => NarOrder::LastFetchedAt,
        _ => NarOrder::CreatedAt,
    };
    let filter = NarFilter {
        cache: cache.id,
        hash_prefix: q.hash.as_deref().filter(|s| !s.is_empty()),
        package: q.package.as_deref().filter(|s| !s.is_empty()),
    };

    let total = gradient_db::cached_paths::count_in_cache(&db.web_db, filter).await?;
    let items = gradient_db::cached_paths::page_in_cache(
        &db.web_db, filter, order, ascending, per_page, offset,
    )
    .await?;

    Ok(ok_json(Paginated {
        items,
        total,
        page,
        per_page,
    }))
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/nars/{hash}",
    operation_id = "getCacheNar",
    tag = "caches",
    summary = "Get NAR detail",
    description = "Returns full metadata for a NAR signed by this cache, including references,\nderiver, content-addressed flag, fetch count, and signature presence.\nPublic caches allow anonymous access; otherwise requires `readStore`.",
    params(CachePath, ("hash" = String, Path)),
    responses((status = OK, description = "NAR detail", body = NarDetail), WebError)
)]
pub async fn show(
    CacheScope {
        cache, db, params, ..
    }: CacheScope<Readable>,
) -> WebResult<Json<BaseResponse<NarDetail>>> {
    let hash: String = params.param("hash")?;

    let cp = gradient_db::cached_paths::by_hash(&db.web_db, &hash)
        .await?
        .or_not_found(Resource::Path)?;
    let sig = gradient_db::cached_paths::signature_in_cache(&db.web_db, cp.id, cache.id)
        .await?
        .or_not_found(Resource::Signature)?;
    let references =
        gradient_db::graph::runtime_closure::references_for_hash(&db.web_db, &hash).await?;
    Ok(ok_json(NarDetail::new(cp, sig, references)))
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/nars/stats",
    operation_id = "getCacheNarStats",
    tag = "caches",
    summary = "NAR aggregate statistics",
    description = "Returns aggregate counts and sizes for all NARs signed by this cache.\nPublic caches allow anonymous access; otherwise requires `readStore`.",
    params(CachePath),
    responses((status = OK, description = "NAR aggregates", body = NarStats), WebError)
)]
pub async fn stats(
    CacheScope { cache, db, .. }: CacheScope<Readable>,
) -> WebResult<Json<BaseResponse<NarStats>>> {
    let row = gradient_db::cached_paths::totals_of_cache(&db.web_db, cache.id)
        .await?
        .context("stats query returned no row")?;

    Ok(ok_json(NarStats::from(row)))
}

#[utoipa::path(
    get,
    path = "/caches/{cache}/nars/available",
    operation_id = "getCacheNarAvailable",
    tag = "caches",
    summary = "Check whether a NAR is available",
    description = "Returns `{ available: true }` when this cache currently holds a signature\nrow for the given store-path hash. Cheaper than a `GET /nars/{hash}` round-trip\nfor clients that only need a yes/no answer.",
    params(CachePath, ("hash" = String, Query)),
    responses((status = OK, description = "Availability flag", body = NarAvailable), WebError)
)]
pub async fn available(
    CacheScope { cache, db, .. }: CacheScope<Readable>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<NarAvailable>>> {
    let hash = q.get("hash").cloned().unwrap_or_default();
    if hash.is_empty() {
        return Err(CacheRefusal::MissingHashQuery.into());
    }
    let cp = gradient_db::cached_paths::by_hash(&db.web_db, &hash).await?;
    let exists = if let Some(cp) = cp {
        gradient_db::cached_paths::signature_in_cache(&db.web_db, cp.id, cache.id)
            .await?
            .is_some()
    } else {
        false
    };
    Ok(ok_json(NarAvailable { available: exists }))
}

#[utoipa::path(
    delete,
    path = "/caches/{cache}/nars/{hash}",
    operation_id = "deleteCacheNar",
    tag = "caches",
    summary = "Delete a NAR from this cache",
    description = "Removes this cache's signature row for the given store-path hash. The delete\nis ref-counted: when other caches still hold the same NAR, only this cache's\nsignature is dropped. When this is the last cache holding the NAR, the\n`cached_path` row is removed in the same transaction, any matching\n`derivation_output.is_cached` rows are flipped to `false`, and the NAR\nblob is garbage-collected asynchronously after the response. Requires\n`writeStore`. Allowed on state-managed caches - NAR content is operational\ndata, distinct from cache configuration.",
    params(CachePath, ("hash" = String, Path)),
    responses((status = NO_CONTENT, description = "NAR removed from this cache"), WebError)
)]
pub async fn delete(
    CacheScope { cache, params, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
) -> WebResult<impl IntoResponse> {
    let hash: String = params.param("hash")?;

    let (cp, outcome) =
        delete_nar_from_cache(&state.nar_storage, &state.graph, cache.id, &hash).await?;
    audit.write(Action::CacheNarDelete, EventOwner::cache(cache.id), serde_json::json!({ "cache_id": cache.id.to_string(), "cache_name": cache.name, "hash": hash, "store_path": cp.store_path(), "ref_counted_others": outcome.ref_counted_others, }))
    .await;
    Ok(StatusCode::NO_CONTENT)
}
