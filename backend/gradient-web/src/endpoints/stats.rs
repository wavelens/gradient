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
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::cache_metric;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::Serialize;
use std::sync::Arc;

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

/// Add bytes served for a NAR request to the current minute bucket of the
/// per-instance accumulator. `gradient_db::cache_metric` writes it; the request
/// path never touches the `cache_metric` row (#644).
pub fn record_nar_traffic(state: &ServerState, cache_id: CacheId, bytes: i64) {
    let bucket = cache_metric::minute_bucket(gradient_types::now());
    state.cache_traffic.record(cache_id, bucket, bytes);
}

/// Zero-filled time-series of a cache rollup metric. `count` and `sum` carry
/// the two values the cache-stats UI needs (requests/bytes or packages/bytes).
async fn cache_series<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: CacheId,
    metric: &str,
    granularity: RollupGranularity,
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
        unit = granularity.trunc_unit(),
        back = back_interval,
        gran = i16::from(granularity),
    );

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            [
                sea_orm::Value::String(Some(metric.to_owned())),
                sea_orm::Value::String(Some(cache_id.to_string())),
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
    granularity: RollupGranularity,
    back_interval: &str,
) -> Result<Vec<CacheMetricPoint>, WebError> {
    let series = cache_series(db, cache_id, "cache.bytes_sent", granularity, back_interval).await?;
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
    granularity: RollupGranularity,
    back_interval: &str,
) -> Result<Vec<StorageMetricPoint>, WebError> {
    let series = cache_series(
        db,
        cache_id,
        "cache.bytes_added",
        granularity,
        back_interval,
    )
    .await?;
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
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COALESCE(SUM(cp.file_size), 0)::bigint AS total_bytes,
                      COALESCE(SUM(cp.nar_size),  0)::bigint AS total_nar_bytes,
                      COUNT(cps.id)::bigint                   AS total_packages
               FROM cached_path_signature cps
               JOIN cached_path cp ON cp.id = cps.cached_path
               WHERE cps.cache = $1"#,
            [sea_orm::Value::Uuid(Some(cache.id.into_inner()))],
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
            aggregate_storage(
                &state.web_db,
                cache.id,
                RollupGranularity::Minute,
                "59 minutes"
            ),
            aggregate_storage(&state.web_db, cache.id, RollupGranularity::Hour, "23 hours"),
            aggregate_storage(&state.web_db, cache.id, RollupGranularity::Day, "29 days"),
            aggregate_storage(&state.web_db, cache.id, RollupGranularity::Week, "11 weeks"),
            aggregate_traffic(
                &state.web_db,
                cache.id,
                RollupGranularity::Minute,
                "59 minutes"
            ),
            aggregate_traffic(&state.web_db, cache.id, RollupGranularity::Hour, "23 hours"),
            aggregate_traffic(&state.web_db, cache.id, RollupGranularity::Day, "29 days"),
            aggregate_traffic(&state.web_db, cache.id, RollupGranularity::Week, "11 weeks"),
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
    use chrono::Timelike;
    use sea_orm::MockDatabase;

    /// The serving path must only add into the accumulator: it takes no
    /// connection, so the `cache_metric` row cannot be written per NAR (#644).
    /// That the accumulated bucket becomes one additive upsert is
    /// `gradient_db::cache_metric`'s test.
    #[tokio::test]
    async fn two_serves_accumulate_instead_of_writing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = gradient_test_support::state::test_state(db);

        record_nar_traffic(&state, CacheId::nil(), 4096);
        record_nar_traffic(&state, CacheId::nil(), 1024);

        let taken = state.cache_traffic.take();
        let bytes: i64 = taken.iter().map(|(_, _, t)| t.bytes).sum();
        let nars: i64 = taken.iter().map(|(_, _, t)| t.nars).sum();

        assert_eq!((bytes, nars), (5120, 2), "both serves counted: {taken:?}");
        assert!(
            taken.iter().all(|(_, bucket, _)| bucket.second() == 0),
            "a serve is bucketed by its minute: {taken:?}"
        );
        assert!(
            state.cache_traffic.take().is_empty(),
            "a flush leaves nothing behind"
        );
    }
}
