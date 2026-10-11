/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter,
    Value,
};
use serde::Serialize;

pub async fn add_missing_signatures<C: ConnectionTrait>(
    db: &C,
    signatures: Vec<ACachedPathSignature>,
) -> Result<(), DbErr> {
    ECachedPathSignature::insert_many(signatures)
        .on_conflict(
            OnConflict::columns([
                CCachedPathSignature::CachedPath,
                CCachedPathSignature::Cache,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(db)
        .await?;
    Ok(())
}

pub async fn by_hash<C: ConnectionTrait>(db: &C, hash: &str) -> Result<Option<MCachedPath>, DbErr> {
    ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await
}

pub async fn by_any_file_hash<C: ConnectionTrait>(
    db: &C,
    file_hashes: Vec<String>,
) -> Result<Option<MCachedPath>, DbErr> {
    ECachedPath::find()
        .filter(CCachedPath::FileHash.is_in(file_hashes))
        .one(db)
        .await
}

pub async fn signature_in_cache<C: ConnectionTrait>(
    db: &C,
    cached_path: CachedPathId,
    cache: CacheId,
) -> Result<Option<MCachedPathSignature>, DbErr> {
    ECachedPathSignature::find()
        .filter(
            Condition::all()
                .add(CCachedPathSignature::CachedPath.eq(cached_path))
                .add(CCachedPathSignature::Cache.eq(cache)),
        )
        .one(db)
        .await
}

crate::sql! {
    CACHE_SERVES_PATH = "SELECT 1 AS served \
         FROM cached_path cp \
         JOIN cached_path_signature s ON s.cached_path = cp.id \
         WHERE cp.hash = $1 AND s.cache = $2 \
           AND s.signature IS NOT NULL AND cp.file_hash IS NOT NULL",
        params = [CachedPathHash, CacheId];

    CACHE_SERVED_DERIVATION = "SELECT d.id AS id \
         FROM derivation d \
         JOIN derivation_output o ON o.derivation = d.id \
         JOIN cached_path cp ON cp.hash = o.hash \
         JOIN cached_path_signature s ON s.cached_path = cp.id \
         WHERE d.hash = $1 AND d.name = $2 AND s.cache = $3 \
           AND s.signature IS NOT NULL AND cp.file_hash IS NOT NULL \
         LIMIT 1",
        params = [DerivationHash, Text("hello"), CacheId];

    TOUCH_CACHED_PATH_SIGNATURE_FETCH = "UPDATE cached_path_signature \
         SET last_fetched_at = $1, fetch_count = fetch_count + 1 \
         WHERE cache = $2 \
           AND cached_path = (SELECT id FROM cached_path WHERE hash = $3)",
        params = [Now, CacheId, CachedPathHash];

    NAR_STATS = "SELECT COUNT(*) AS total_nars, \
                COALESCE(SUM(cp.nar_size),0)::bigint AS total_nar_size, \
                COALESCE(SUM(cp.file_size),0)::bigint AS total_file_size, \
                MAX(cp.created_at) AS last_uploaded_at, \
                MIN(cps.last_fetched_at) AS oldest_fetched_at \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE cps.cache = $1",
        params = [CacheId],
        tier = Bulk;
}

/// Blobs are living in one store for every cache. This signed claim is all that is keeping one
/// cache's paths out of another's.
pub async fn signed_in_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    store_hash: &str,
) -> Result<bool, DbErr> {
    Ok(db
        .query_one_raw(CACHE_SERVES_PATH.bind([store_hash.into(), cache.into_inner().into()]))
        .await?
        .is_some())
}

pub async fn derivation_signed_in_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    drv_hash: &str,
    drv_name: &str,
) -> Result<Option<DerivationId>, DbErr> {
    let row = db
        .query_one_raw(CACHE_SERVED_DERIVATION.bind([
            drv_hash.into(),
            drv_name.into(),
            cache.into_inner().into(),
        ]))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new))
}

pub async fn count_fetch<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    hash: &str,
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    let now_val = Value::ChronoDateTimeUtc(Some(chrono::DateTime::from_naive_utc_and_offset(
        now,
        chrono::Utc,
    )));
    let cache_val = Value::Uuid(Some(cache.into_inner()));
    let hash_val = Value::String(Some(hash.to_owned()));

    db.execute_raw(TOUCH_CACHED_PATH_SIGNATURE_FETCH.bind([now_val, cache_val, hash_val]))
        .await?;
    Ok(())
}

#[derive(Debug, FromQueryResult)]
pub struct CacheTotals {
    pub total_nars: i64,
    pub total_nar_size: Option<i64>,
    pub total_file_size: Option<i64>,
    pub last_uploaded_at: Option<NaiveDateTime>,
    pub oldest_fetched_at: Option<NaiveDateTime>,
}

pub async fn totals_of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Option<CacheTotals>, DbErr> {
    CacheTotals::find_by_statement(NAR_STATS.bind([Value::Uuid(Some(cache.into_inner()))]))
        .one(db)
        .await
}

fn nars_count_sql(where_sql: &str) -> String {
    format!(
        "SELECT COUNT(*) AS total \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE {where_sql}"
    )
}

fn nars_select_sql(
    where_sql: &str,
    sort_col: &str,
    order_dir: &str,
    limit_idx: usize,
    offset_idx: usize,
) -> String {
    format!(
        "SELECT cp.hash, cp.hash || '-' || cp.package AS store_path, cp.package, \
                cp.nar_size, cp.file_size, cp.created_at, cps.last_fetched_at \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE {where_sql} \
         ORDER BY {sort_col} {order_dir} NULLS LAST, cp.id ASC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}"
    )
}

// The package filter is a substring match that no index can answer. The cache scan is the plan, not
// a missing index.
crate::sql_fn! {
    NARS_LIST_COUNT = || nars_count_sql("cps.cache = $1 AND cp.hash LIKE $2 AND cp.package LIKE $3"),
        params = [CacheId, Text("abc%"), Text("%hello%")],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().seq_scan_allowed();

    NARS_LIST_SELECT = || nars_select_sql(
        "cps.cache = $1 AND cp.hash LIKE $2 AND cp.package LIKE $3",
        "cp.created_at",
        "DESC",
        4,
        5,
    ),
        params = [CacheId, Text("abc%"), Text("%hello%"), Int(50), Int(0)],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().seq_scan_allowed();
}

#[derive(Debug, Clone, Copy)]
pub struct NarFilter<'a> {
    pub cache: CacheId,
    pub hash_prefix: Option<&'a str>,
    pub package: Option<&'a str>,
}

impl NarFilter<'_> {
    fn conditions(&self) -> (String, Vec<Value>) {
        let mut where_clauses = vec!["cps.cache = $1".to_string()];
        let mut values: Vec<Value> = vec![Value::Uuid(Some(self.cache.into_inner()))];

        if let Some(prefix) = self.hash_prefix {
            let n = values.len() + 1;
            where_clauses.push(format!("cp.hash LIKE ${n}"));
            values.push(Value::String(Some(format!("{prefix}%"))));
        }
        if let Some(needle) = self.package {
            let n = values.len() + 1;
            where_clauses.push(format!("cp.package LIKE ${n}"));
            values.push(Value::String(Some(format!("%{needle}%"))));
        }

        (where_clauses.join(" AND "), values)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum NarOrder {
    CreatedAt,
    NarSize,
    LastFetchedAt,
}

impl NarOrder {
    fn column(self) -> &'static str {
        match self {
            Self::CreatedAt => "cp.created_at",
            Self::NarSize => "cp.nar_size",
            Self::LastFetchedAt => "cps.last_fetched_at",
        }
    }
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

pub async fn count_in_cache<C: ConnectionTrait>(
    db: &C,
    filter: NarFilter<'_>,
) -> Result<u64, DbErr> {
    #[derive(FromQueryResult)]
    struct CountRow {
        total: i64,
    }

    let (where_sql, values) = filter.conditions();
    Ok(
        CountRow::find_by_statement(NARS_LIST_COUNT.bind_built(nars_count_sql(&where_sql), values))
            .one(db)
            .await?
            .map(|r| r.total.max(0) as u64)
            .unwrap_or(0),
    )
}

pub async fn page_in_cache<C: ConnectionTrait>(
    db: &C,
    filter: NarFilter<'_>,
    order: NarOrder,
    ascending: bool,
    limit: u64,
    offset: u64,
) -> Result<Vec<NarSummary>, DbErr> {
    let (where_sql, mut values) = filter.conditions();
    let order_dir = if ascending { "ASC" } else { "DESC" };
    let limit_idx = values.len() + 1;
    let offset_idx = values.len() + 2;
    let select_sql = nars_select_sql(&where_sql, order.column(), order_dir, limit_idx, offset_idx);
    values.push(Value::BigInt(Some(limit as i64)));
    values.push(Value::BigInt(Some(offset as i64)));

    NarSummary::find_by_statement(NARS_LIST_SELECT.bind_built(select_sql, values))
        .all(db)
        .await
}

crate::sql! {
    CACHE_TOTALS = r#"SELECT COALESCE(SUM(cp.file_size), 0)::bigint AS total_bytes,
                      COALESCE(SUM(cp.nar_size),  0)::bigint AS total_nar_bytes,
                      COUNT(cps.id)::bigint                   AS total_packages
               FROM cached_path_signature cps
               JOIN cached_path cp ON cp.id = cps.cached_path
               WHERE cps.cache = $1"#,
        params = [CacheId],
        tier = Bulk;
}

pub struct CacheSize {
    pub total_bytes: i64,
    pub total_nar_bytes: i64,
    pub total_packages: i64,
}

pub async fn size_of_cache<C: ConnectionTrait>(db: &C, cache: CacheId) -> Result<CacheSize, DbErr> {
    let row = db
        .query_one_raw(CACHE_TOTALS.bind([Value::Uuid(Some(cache.into_inner()))]))
        .await?;
    let total = |column: &str| {
        row.as_ref()
            .and_then(|row| row.try_get::<i64>("", column).ok())
            .unwrap_or(0)
    };

    Ok(CacheSize {
        total_bytes: total("total_bytes"),
        total_nar_bytes: total("total_nar_bytes"),
        total_packages: total("total_packages"),
    })
}
