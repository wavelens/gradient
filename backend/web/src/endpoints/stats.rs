/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::helpers::ok_json;
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{NaiveDateTime, Timelike};
use core::db::get_any_cache_by_name;
use core::types::*;
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
pub async fn record_nar_traffic(state: Arc<ServerState>, cache_id: Uuid, bytes: i64) {
    let now = core::types::now();
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
fn build_record_nar_traffic_stmt(cache_id: Uuid, bucket: NaiveDateTime, bytes: i64) -> Statement {
    Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"INSERT INTO cache_metric (id, cache, bucket_time, bytes_sent, nar_count)
           VALUES ($1, $2, $3, $4, 1)
           ON CONFLICT (cache, bucket_time)
           DO UPDATE SET bytes_sent = cache_metric.bytes_sent + EXCLUDED.bytes_sent,
                         nar_count  = cache_metric.nar_count  + EXCLUDED.nar_count"#,
        [
            sea_orm::Value::Uuid(Some(Box::new(Uuid::new_v4()))),
            sea_orm::Value::Uuid(Some(Box::new(cache_id))),
            sea_orm::Value::ChronoDateTime(Some(Box::new(bucket))),
            sea_orm::Value::BigInt(Some(bytes)),
        ],
    )
}

async fn aggregate_traffic<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: Uuid,
    trunc_unit: &str,
    back_interval: &str,
) -> Result<Vec<CacheMetricPoint>, WebError> {
    // Use generate_series so every bucket in the window is present, including the
    // current one — this makes the traffic graph always run to "now" with zeros.
    let sql = format!(
        r#"SELECT gs.period,
                  COALESCE(SUM(cm.bytes_sent), 0)::bigint AS bytes,
                  COALESCE(SUM(cm.nar_count),  0)::bigint AS requests
           FROM generate_series(
               date_trunc('{trunc_unit}', NOW() AT TIME ZONE 'UTC') - INTERVAL '{back_interval}',
               date_trunc('{trunc_unit}', NOW() AT TIME ZONE 'UTC'),
               INTERVAL '1 {trunc_unit}'
           ) AS gs(period)
           LEFT JOIN cache_metric cm
               ON date_trunc('{trunc_unit}', cm.bucket_time) = gs.period
              AND cm.cache = $1
           GROUP BY gs.period
           ORDER BY gs.period"#,
        trunc_unit = trunc_unit,
        back_interval = back_interval,
    );

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            [sea_orm::Value::Uuid(Some(Box::new(cache_id)))],
        ))
        .await
        .map_err(WebError::from)?;

    let points = rows
        .into_iter()
        .filter_map(|row| {
            let time: NaiveDateTime = row.try_get("", "period").ok()?;
            let bytes: i64 = row.try_get("", "bytes").unwrap_or(0);
            let requests: i64 = row.try_get("", "requests").unwrap_or(0);
            Some(CacheMetricPoint {
                time: time.to_string(),
                bytes,
                requests,
            })
        })
        .collect();

    Ok(points)
}

async fn aggregate_storage<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: Uuid,
    trunc_unit: &str,
    back_interval: &str,
) -> Result<Vec<StorageMetricPoint>, WebError> {
    let sql = format!(
        r#"SELECT gs.period,
                  COALESCE(COUNT(cps.id),      0)::bigint AS packages,
                  COALESCE(SUM(cp.file_size),  0)::bigint AS bytes
           FROM generate_series(
               date_trunc('{trunc_unit}', NOW() AT TIME ZONE 'UTC') - INTERVAL '{back_interval}',
               date_trunc('{trunc_unit}', NOW() AT TIME ZONE 'UTC'),
               INTERVAL '1 {trunc_unit}'
           ) AS gs(period)
           LEFT JOIN cached_path_signature cps
               ON date_trunc('{trunc_unit}', cps.created_at) = gs.period
              AND cps.cache = $1
           LEFT JOIN cached_path cp ON cp.id = cps.cached_path
           GROUP BY gs.period
           ORDER BY gs.period"#,
        trunc_unit = trunc_unit,
        back_interval = back_interval,
    );

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            [sea_orm::Value::Uuid(Some(Box::new(cache_id)))],
        ))
        .await
        .map_err(WebError::from)?;

    let points = rows
        .into_iter()
        .filter_map(|row| {
            let time: NaiveDateTime = row.try_get("", "period").ok()?;
            let packages: i64 = row.try_get("", "packages").unwrap_or(0);
            let bytes: i64 = row.try_get("", "bytes").unwrap_or(0);
            Some(StorageMetricPoint {
                time: time.to_string(),
                packages,
                bytes,
            })
        })
        .collect();

    Ok(points)
}

pub async fn get_cache_stats(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(cache): Path<String>,
) -> WebResult<Json<BaseResponse<CacheStatsResponse>>> {
    let cache = get_any_cache_by_name(state.0.clone(), cache)
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    if !cache.public {
        match &maybe_user {
            Some(user) if cache.created_by == user.id => {}
            _ => return Err(WebError::not_found("Cache")),
        }
    }

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
            [sea_orm::Value::Uuid(Some(Box::new(cache.id)))],
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
    /// concurrently — both reads saw the same `bytes_sent` and the second
    /// write clobbered the first.
    #[test]
    fn record_nar_traffic_stmt_is_atomic_upsert() {
        let bucket = NaiveDate::from_ymd_opt(2026, 5, 2)
            .unwrap()
            .and_hms_opt(12, 34, 0)
            .unwrap();
        let cache_id = Uuid::nil();

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
