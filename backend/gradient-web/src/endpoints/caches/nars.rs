/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tooling-facing NAR management surface under `/api/v1/caches/{cache}/nars`.

use super::helpers::delete_nar_from_cache;
use crate::access::{CacheAccess, Caller, load_cache};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::CachePermission;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::NaiveDateTime;
use gradient_core::types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, FromQueryResult, QueryFilter};
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

#[derive(Debug, Serialize, FromQueryResult)]
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

pub async fn list(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache_name): Path<String>,
    Query(q): Query<ListQuery>,
) -> WebResult<Json<BaseResponse<NarListResponse>>> {
    use sea_orm::{DatabaseBackend, Statement};

    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Readable,
    )
    .await?;
    let per_page = q
        .per_page
        .unwrap_or(DEFAULT_PER_PAGE)
        .clamp(1, MAX_PER_PAGE);
    let page = q.page.unwrap_or(1).max(1);
    let offset = (page - 1) * per_page;

    let ascending = matches!(q.order.as_deref(), Some("asc"));
    let order_dir = if ascending { "ASC" } else { "DESC" };
    let sort_col = match q.sort.as_deref().unwrap_or("created_at") {
        "nar_size" => "cp.nar_size",
        "last_fetched_at" => "cps.last_fetched_at",
        _ => "cp.created_at",
    };

    let mut where_clauses = vec!["cps.cache = $1".to_string()];
    let mut values: Vec<sea_orm::Value> =
        vec![sea_orm::Value::Uuid(Some(Box::new(cache.id.into_inner())))];

    if let Some(prefix) = q.hash.as_deref().filter(|s| !s.is_empty()) {
        let n = values.len() + 1;
        where_clauses.push(format!("cp.hash LIKE ${n}"));
        values.push(sea_orm::Value::String(Some(Box::new(format!("{prefix}%")))));
    }
    if let Some(needle) = q.package.as_deref().filter(|s| !s.is_empty()) {
        let n = values.len() + 1;
        where_clauses.push(format!("cp.package LIKE ${n}"));
        values.push(sea_orm::Value::String(Some(Box::new(format!(
            "%{needle}%"
        )))));
    }
    let where_sql = where_clauses.join(" AND ");

    #[derive(FromQueryResult)]
    struct CountRow {
        total: i64,
    }

    let count_sql = format!(
        "SELECT COUNT(*) AS total \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE {where_sql}"
    );
    let total = CountRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        &count_sql,
        values.clone(),
    ))
    .one(&state.web_db)
    .await?
    .map(|r| r.total.max(0) as u64)
    .unwrap_or(0);

    // `NULLS LAST` keeps unsigned/never-fetched rows from monopolising the
    // first page on `DESC` sorts; `cp.id` is the stable tie-breaker so
    // separate page fetches return disjoint sets.
    let limit_idx = values.len() + 1;
    let offset_idx = values.len() + 2;
    let select_sql = format!(
        "SELECT cp.hash, cp.store_path, cp.package, cp.nar_size, cp.file_size, \
                cp.created_at, cps.last_fetched_at \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE {where_sql} \
         ORDER BY {sort_col} {order_dir} NULLS LAST, cp.id ASC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}"
    );
    values.push(sea_orm::Value::BigInt(Some(per_page as i64)));
    values.push(sea_orm::Value::BigInt(Some(offset as i64)));

    let items = NarSummary::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        &select_sql,
        values,
    ))
    .all(&state.web_db)
    .await?;

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
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache_name, hash)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<NarDetail>>> {
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Readable,
    )
    .await?;
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache_name): Path<String>,
) -> WebResult<Json<BaseResponse<NarStats>>> {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Readable,
    )
    .await?;

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
                COALESCE(SUM(cp.nar_size),0)::bigint AS total_nar_size, \
                COALESCE(SUM(cp.file_size),0)::bigint AS total_file_size, \
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache_name): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<NarAvailable>>> {
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache_name,
        CacheAccess::Readable,
    )
    .await?;
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path((cache_name, hash)): Path<(String, String)>,
) -> WebResult<impl IntoResponse> {
    let cache = load_cache(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        cache_name.clone(),
        CacheAccess::Require {
            permission: CachePermission::WriteStore,
            reject_managed: false,
        },
    )
    .await?;
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
