/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{CacheAccess, Caller, load_cache};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{NaiveDateTime, Timelike};
use gradient_core::types::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct CacheMetricPoint {
    pub time: String,
    pub bytes: i64,
    pub requests: i64,
}

#[derive(Serialize)]
pub struct StorageMetricPoint {
    pub time: String,
    /// Packages added to the cache in this bucket.
    pub packages: i64,
    /// Compressed bytes added in this bucket.
    pub bytes: i64,
}

#[derive(Serialize)]
pub struct CacheStatsResponse {
    /// Total compressed bytes of all NARs cached by this cache.
    pub total_bytes: i64,
    /// Total uncompressed NAR bytes of all packages cached by this cache.
    pub total_nar_bytes: i64,
    /// Total number of packages (signed build outputs) in this cache.
    pub total_packages: i64,
    /// Packages/bytes added per minute for the last 60 minutes.
    pub storage_minutes: Vec<StorageMetricPoint>,
    /// Packages/bytes added per hour for the last 24 hours.
    pub storage_hours: Vec<StorageMetricPoint>,
    /// Packages/bytes added per day for the last 30 days.
    pub storage_days: Vec<StorageMetricPoint>,
    /// Packages/bytes added per week for the last 12 weeks.
    pub storage_weeks: Vec<StorageMetricPoint>,
    /// Traffic bucketed by minute for the last 60 minutes.
    pub minutes: Vec<CacheMetricPoint>,
    /// Traffic bucketed by hour for the last 24 hours.
    pub hours: Vec<CacheMetricPoint>,
    /// Traffic bucketed by day for the last 30 days.
    pub days: Vec<CacheMetricPoint>,
    /// Traffic bucketed by week for the last 12 weeks.
    pub weeks: Vec<CacheMetricPoint>,
}

/// Record bytes served for a NAR request into the current minute bucket.
/// Called fire-and-forget from the NAR serving handler.
pub async fn record_nar_traffic(state: Arc<ServerState>, cache_id: CacheId, bytes: i64) {
    let now = gradient_core::types::now();
    let bucket = match now.with_second(0).and_then(|t| t.with_nanosecond(0)) {
        Some(t) => t,
        None => now,
    };

    let stmt = build_record_nar_traffic_stmt(cache_id, bucket, bytes);
    if let Err(e) = state.web_db.execute(stmt).await {
        tracing::warn!(error = %e, "Failed to record cache metric");
    }
}

/// Build the atomic UPSERT statement that records one NAR request into the
/// `(cache, bucket_time)` row. Concurrent calls into the same bucket are
/// serialised by Postgres on the unique `(cache, bucket_time)` index, so each
/// caller's `bytes_sent`/`nar_count` increment is preserved (no lost updates).
fn build_record_nar_traffic_stmt(
    cache_id: CacheId,
    bucket: NaiveDateTime,
    bytes: i64,
) -> Statement {
    Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO cache_metric (id, cache, bucket_time, bytes_sent, nar_count)
           VALUES ($1, $2, $3, $4, 1)
           ON CONFLICT (cache, bucket_time)
           DO UPDATE SET bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent,
                         nar_count  = cache_metric.nar_count  + EXCLUDED.nar_count"#,
        [
            sea_orm::Value::Uuid(Some(Box::new(Uuid::now_v7()))),
            sea_orm::Value::Uuid(Some(Box::new(cache_id.into_inner()))),
            sea_orm::Value::ChronoDateTime(Some(Box::new(bucket))),
            sea_orm::Value::BigInt(Some(bytes)),
        ],
    )
}

/// Granularity discriminant matching `metric_rollup.granularity`.
fn gran_code(trunc_unit: &str) -> i16 {
    match trunc_unit {
        "hour" => 1,
        "day" => 2,
        "week" => 3,
        _ => 0,
    }
}

/// Zero-filled time-series of a cache rollup metric. `count` and `sum` carry
/// the two values the cache-stats UI needs (requests/bytes or packages/bytes).
async fn cache_series<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: CacheId,
    metric: &str,
    trunc_unit: &str,
    back_interval: &str,
) -> Result<Vec<(NaiveDateTime, i64, i64)>, WebError> {
    // generate_series keeps every bucket present (zero-filled) up to "now"; the
    // values come from the new metric_rollup aggregates rather than ad-hoc scans.
    let sql = format!(
        r#"SELECT gs.period,
                  COALESCE(SUM(mr.count), 0)::bigint AS cnt,
                  COALESCE(SUM(mr.sum), 0)::bigint    AS total
           FROM generate_series(
               date_trunc('{unit}', NOW() AT TIME ZONE 'UTC') - INTERVAL '{back}',
               date_trunc('{unit}', NOW() AT TIME ZONE 'UTC'),
               INTERVAL '1 {unit}'
           ) AS gs(period)
           LEFT JOIN metric_rollup mr
               ON mr.bucket_start = gs.period
              AND mr.metric = $1
              AND mr.granularity = {gran}
              AND (mr.scope->>'cache') = $2
           GROUP BY gs.period
           ORDER BY gs.period"#,
        unit = trunc_unit,
        back = back_interval,
        gran = gran_code(trunc_unit),
    );

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            [
                sea_orm::Value::String(Some(Box::new(metric.to_owned()))),
                sea_orm::Value::String(Some(Box::new(cache_id.to_string()))),
            ],
        ))
        .await
        .map_err(WebError::from)?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let time: NaiveDateTime = row.try_get("", "period").ok()?;
            let cnt: i64 = row.try_get("", "cnt").unwrap_or(0);
            let total: i64 = row.try_get("", "total").unwrap_or(0);
            Some((time, cnt, total))
        })
        .collect())
}

async fn aggregate_traffic<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: CacheId,
    trunc_unit: &str,
    back_interval: &str,
) -> Result<Vec<CacheMetricPoint>, WebError> {
    let series = cache_series(db, cache_id, "cache.bytes_sent", trunc_unit, back_interval).await?;
    Ok(series
        .into_iter()
        .map(|(time, requests, bytes)| CacheMetricPoint {
            time: time.to_string(),
            bytes,
            requests,
        })
        .collect())
}

async fn aggregate_storage<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: CacheId,
    trunc_unit: &str,
    back_interval: &str,
) -> Result<Vec<StorageMetricPoint>, WebError> {
    let series = cache_series(db, cache_id, "cache.bytes_added", trunc_unit, back_interval).await?;
    Ok(series
        .into_iter()
        .map(|(time, packages, bytes)| StorageMetricPoint {
            time: time.to_string(),
            packages,
            bytes,
        })
        .collect())
}

pub async fn get_cache_stats(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheStatsResponse>>> {
    let cache = load_cache(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;

    // Total compressed bytes and package count for this cache.
    let total_row = state
        .web_db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COALESCE(SUM(cp.file_size), 0)::bigint AS total_bytes,
                      COALESCE(SUM(cp.nar_size),  0)::bigint AS total_nar_bytes,
                      COUNT(cps.id)::bigint                   AS total_packages
               FROM cached_path_signature cps
               JOIN cached_path cp ON cp.id = cps.cached_path
               WHERE cps.cache = $1"#,
            [sea_orm::Value::Uuid(Some(Box::new(cache.id.into_inner())))],
        ))
        .await
        .map_err(WebError::from)?;

    let total_bytes: i64 = total_row
        .as_ref()
        .and_then(|row| row.try_get::<i64>("", "total_bytes").ok())
        .unwrap_or(0);

    let total_nar_bytes: i64 = total_row
        .as_ref()
        .and_then(|row| row.try_get::<i64>("", "total_nar_bytes").ok())
        .unwrap_or(0);

    let total_packages: i64 = total_row
        .as_ref()
        .and_then(|row| row.try_get::<i64>("", "total_packages").ok())
        .unwrap_or(0);

    let (storage_minutes, storage_hours, storage_days, storage_weeks, minutes, hours, days, weeks) =
        tokio::try_join!(
            aggregate_storage(&state.web_db, cache.id, "minute", "59 minutes"),
            aggregate_storage(&state.web_db, cache.id, "hour", "23 hours"),
            aggregate_storage(&state.web_db, cache.id, "day", "29 days"),
            aggregate_storage(&state.web_db, cache.id, "week", "11 weeks"),
            aggregate_traffic(&state.web_db, cache.id, "minute", "59 minutes"),
            aggregate_traffic(&state.web_db, cache.id, "hour", "23 hours"),
            aggregate_traffic(&state.web_db, cache.id, "day", "29 days"),
            aggregate_traffic(&state.web_db, cache.id, "week", "11 weeks"),
        )?;

    Ok(ok_json(CacheStatsResponse {
        total_bytes,
        total_nar_bytes,
        total_packages,
        storage_minutes,
        storage_hours,
        storage_days,
        storage_weeks,
        minutes,
        hours,
        days,
        weeks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// Regression for #50: the bucket-update path must be a single atomic
    /// UPSERT, not a SELECT-then-UPDATE. The previous implementation lost
    /// updates whenever two NAR fetches landed in the same minute bucket
    /// concurrently - both reads saw the same `bytes_sent` and the second
    /// write clobbered the first.
    #[test]
    fn record_nar_traffic_stmt_is_atomic_upsert() {
        let bucket = NaiveDate::from_ymd_opt(2026, 5, 2)
            .unwrap()
            .and_hms_opt(12, 34, 0)
            .unwrap();
        let cache_id = CacheId::nil();

        let stmt = build_record_nar_traffic_stmt(cache_id, bucket, 4096);
        let sql = stmt.to_string();

        assert!(
            sql.contains("INSERT INTO cache_metric"),
            "expected INSERT, got: {sql}"
        );
        assert!(
            sql.contains("ON CONFLICT (cache, bucket_time)"),
            "expected ON CONFLICT clause inferring the unique index, got: {sql}"
        );
        assert!(
            sql.contains("bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent"),
            "expected additive update on bytes_sent, got: {sql}"
        );
        assert!(
            sql.contains("nar_count  = cache_metric.nar_count  + EXCLUDED.nar_count"),
            "expected additive update on nar_count, got: {sql}"
        );
        assert!(
            !sql.to_uppercase().contains("SELECT"),
            "atomic upsert must not contain a SELECT (would reintroduce the race), got: {sql}"
        );
    }
}
