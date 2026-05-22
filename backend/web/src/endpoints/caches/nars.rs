/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tooling-facing NAR management surface under `/api/v1/caches/{cache}/nars`.

use super::helpers::delete_nar_from_cache;
use crate::access::{CacheAccess, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::NaiveDateTime;
use gradient_core::db::get_any_cache_by_name;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub hash: Option<String>,
    pub package: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub per_page: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct NarSummary {
    pub hash: String,
    pub store_path: String,
    pub package: String,
    pub nar_size: Option<i64>,
    pub file_size: Option<i64>,
    pub created_at: NaiveDateTime,
    pub last_fetched_at: Option<NaiveDateTime>,
}

#[derive(Debug, Serialize)]
pub struct NarListResponse {
    pub items: Vec<NarSummary>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
pub struct NarStats {
    pub total_nars: i64,
    pub total_nar_size: i64,
    pub total_file_size: i64,
    pub last_uploaded_at: Option<NaiveDateTime>,
    pub oldest_fetched_at: Option<NaiveDateTime>,
}

#[derive(Debug, Serialize)]
pub struct NarAvailable {
    pub available: bool,
}

const DEFAULT_PER_PAGE: u64 = 50;
const MAX_PER_PAGE: u64 = 200;

async fn resolve_visible_cache(
    state: &Arc<ServerState>,
    maybe_user: &Option<MUser>,
    cache_name: String,
) -> WebResult<MCache> {
    let cache: MCache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .or_not_found("Cache")?;
    if !cache.public {
        match maybe_user {
            Some(u) if u.id == cache.created_by => {}
            _ => return Err(WebError::not_found("Cache")),
        }
    }
    Ok(cache)
}

pub async fn list(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache_name): Path<String>,
    Query(q): Query<ListQuery>,
) -> WebResult<Json<BaseResponse<NarListResponse>>> {
    let cache = resolve_visible_cache(&state, &maybe_user, cache_name).await?;
    let per_page = q.per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE);
    let page = q.page.unwrap_or(1).max(1);

    let mut paths_query = ECachedPath::find();
    if let Some(prefix) = q.hash.as_deref().filter(|s| !s.is_empty()) {
        paths_query = paths_query.filter(CCachedPath::Hash.starts_with(prefix));
    }
    if let Some(needle) = q.package.as_deref().filter(|s| !s.is_empty()) {
        paths_query = paths_query.filter(CCachedPath::Package.contains(needle));
    }

    let has_filter = q.hash.as_deref().is_some_and(|s| !s.is_empty())
        || q.package.as_deref().is_some_and(|s| !s.is_empty());

    let mut sig_query =
        ECachedPathSignature::find().filter(CCachedPathSignature::Cache.eq(cache.id));

    if has_filter {
        let matching_paths: Vec<CachedPathId> = paths_query
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|p| p.id)
            .collect();
        if matching_paths.is_empty() {
            return Ok(ok_json(NarListResponse {
                items: Vec::new(),
                total: 0,
                page,
                per_page,
            }));
        }
        sig_query = sig_query.filter(CCachedPathSignature::CachedPath.is_in(matching_paths));
    }

    let ascending = matches!(q.order.as_deref(), Some("asc"));
    sig_query = match q.sort.as_deref().unwrap_or("created_at") {
        "last_fetched_at" => {
            if ascending {
                sig_query.order_by_asc(CCachedPathSignature::LastFetchedAt)
            } else {
                sig_query.order_by_desc(CCachedPathSignature::LastFetchedAt)
            }
        }
        _ => sig_query,
    };

    let paginator = sig_query.paginate(&state.web_db, per_page);
    let total = paginator.num_items().await?;
    let signatures = paginator.fetch_page(page - 1).await?;

    let cp_ids: Vec<CachedPathId> = signatures.iter().map(|s| s.cached_path).collect();
    let paths = if cp_ids.is_empty() {
        Vec::new()
    } else {
        ECachedPath::find()
            .filter(CCachedPath::Id.is_in(cp_ids))
            .all(&state.web_db)
            .await?
    };
    let by_id: std::collections::HashMap<CachedPathId, MCachedPath> =
        paths.into_iter().map(|p| (p.id, p)).collect();

    let mut items: Vec<NarSummary> = signatures
        .iter()
        .filter_map(|sig| {
            by_id.get(&sig.cached_path).map(|p| NarSummary {
                hash: p.hash.clone(),
                store_path: p.store_path.clone(),
                package: p.package.clone(),
                nar_size: p.nar_size,
                file_size: p.file_size,
                created_at: p.created_at,
                last_fetched_at: sig.last_fetched_at,
            })
        })
        .collect();

    match q.sort.as_deref().unwrap_or("created_at") {
        "nar_size" => items.sort_by_key(|s| s.nar_size.unwrap_or(0)),
        "created_at" => items.sort_by_key(|s| s.created_at),
        _ => {}
    }
    if !ascending
        && matches!(
            q.sort.as_deref().unwrap_or("created_at"),
            "nar_size" | "created_at"
        )
    {
        items.reverse();
    }

    Ok(ok_json(NarListResponse {
        items,
        total,
        page,
        per_page,
    }))
}

pub async fn show(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((cache_name, hash)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<NarDetail>>> {
    let cache = resolve_visible_cache(&state, &maybe_user, cache_name).await?;
    let cp = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(&hash))
        .one(&state.web_db)
        .await?
        .or_not_found("Path")?;
    let sig = ECachedPathSignature::find()
        .filter(CCachedPathSignature::Cache.eq(cache.id))
        .filter(CCachedPathSignature::CachedPath.eq(cp.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Signature")?;
    Ok(ok_json(NarDetail {
        hash: cp.hash,
        store_path: cp.store_path,
        package: cp.package,
        nar_size: cp.nar_size,
        file_size: cp.file_size,
        file_hash: cp.file_hash,
        nar_hash: cp.nar_hash,
        references: cp
            .references
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        deriver: cp.deriver,
        ca: cp.ca,
        created_at: cp.created_at,
        last_fetched_at: sig.last_fetched_at,
        fetch_count: sig.fetch_count,
        signed: sig.signature.is_some(),
    }))
}

pub async fn stats(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache_name): Path<String>,
) -> WebResult<Json<BaseResponse<NarStats>>> {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
    let cache = resolve_visible_cache(&state, &maybe_user, cache_name).await?;

    #[derive(FromQueryResult)]
    struct Row {
        total_nars: i64,
        total_nar_size: Option<i64>,
        total_file_size: Option<i64>,
        last_uploaded_at: Option<NaiveDateTime>,
        oldest_fetched_at: Option<NaiveDateTime>,
    }

    let row = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT COUNT(*) AS total_nars, \
                COALESCE(SUM(cp.nar_size),0) AS total_nar_size, \
                COALESCE(SUM(cp.file_size),0) AS total_file_size, \
                MAX(cp.created_at) AS last_uploaded_at, \
                MIN(cps.last_fetched_at) AS oldest_fetched_at \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE cps.cache = $1",
        [sea_orm::Value::Uuid(Some(Box::new(cache.id.into_inner())))],
    ))
    .one(&state.web_db)
    .await?
    .ok_or_else(|| WebError::internal("stats query returned no row"))?;

    Ok(ok_json(NarStats {
        total_nars: row.total_nars,
        total_nar_size: row.total_nar_size.unwrap_or(0),
        total_file_size: row.total_file_size.unwrap_or(0),
        last_uploaded_at: row.last_uploaded_at,
        oldest_fetched_at: row.oldest_fetched_at,
    }))
}

pub async fn available(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache_name): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<NarAvailable>>> {
    let cache = resolve_visible_cache(&state, &maybe_user, cache_name).await?;
    let hash = q.get("hash").cloned().unwrap_or_default();
    if hash.is_empty() {
        return Err(WebError::bad_request("missing ?hash="));
    }
    let cp = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(&hash))
        .one(&state.web_db)
        .await?;
    let exists = if let Some(cp) = cp {
        ECachedPathSignature::find()
            .filter(CCachedPathSignature::CachedPath.eq(cp.id))
            .filter(CCachedPathSignature::Cache.eq(cache.id))
            .one(&state.web_db)
            .await?
            .is_some()
    } else {
        false
    };
    Ok(ok_json(NarAvailable { available: exists }))
}

pub async fn delete(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Path((cache_name, hash)): Path<(String, String)>,
) -> WebResult<impl IntoResponse> {
    let cache = load_cache(&state, user.id, cache_name.clone(), CacheAccess::Editable).await?;
    let (cp, outcome) = delete_nar_from_cache(&state, cache.id, &hash).await?;
    audit_record(
        &state.web_db,
        Some(user.id),
        events::CACHE_NAR_DELETE,
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "cache_name": cache.name,
            "hash": hash,
            "store_path": cp.store_path,
            "ref_counted_others": outcome.ref_counted_others,
        })),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
