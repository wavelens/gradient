/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Readable};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::metrics::cache_traffic;
use gradient_db::metrics::cache_traffic::CacheTraffic;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_types::*;
use gradient_util::telemetry::metric;
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
    pub packages: i64,
    pub bytes: i64,
}

#[derive(Serialize)]
pub struct CacheStatsResponse {
    pub total_bytes: i64,
    pub total_nar_bytes: i64,
    pub total_packages: i64,
    pub storage_minutes: Vec<StorageMetricPoint>,
    pub storage_hours: Vec<StorageMetricPoint>,
    pub storage_days: Vec<StorageMetricPoint>,
    pub storage_weeks: Vec<StorageMetricPoint>,
    pub minutes: Vec<CacheMetricPoint>,
    pub hours: Vec<CacheMetricPoint>,
    pub days: Vec<CacheMetricPoint>,
    pub weeks: Vec<CacheMetricPoint>,
}

/// The request path must never touch the `cache_metric` row (#644).
/// `gradient_db::metrics::cache_traffic` is writing the accumulator.
pub fn record_nar_traffic(cache_traffic: &CacheTraffic, cache_id: CacheId, bytes: i64) {
    let bucket = cache_traffic::minute_bucket(gradient_types::now());
    cache_traffic.record(cache_id, bucket, bytes);
}

async fn aggregate_traffic<C: sea_orm::ConnectionTrait>(
    db: &C,
    cache_id: CacheId,
    granularity: RollupGranularity,
    back_interval: &str,
) -> Result<Vec<CacheMetricPoint>, WebError> {
    let series = gradient_db::metrics::series::of_cache(
        db,
        cache_id,
        metric::CACHE_BYTES_SENT,
        granularity,
        back_interval,
    )
    .await
    .map_err(WebError::from)?;
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
    let series = gradient_db::metrics::series::of_cache(
        db,
        cache_id,
        metric::CACHE_BYTES_ADDED,
        granularity,
        back_interval,
    )
    .await
    .map_err(WebError::from)?;
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
    CacheScope { cache, .. }: CacheScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<CacheStatsResponse>>> {
    let size = gradient_db::cached_paths::size_of_cache(&state.web_db, cache.id)
        .await
        .map_err(WebError::from)?;

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
        total_bytes: size.total_bytes,
        total_nar_bytes: size.total_nar_bytes,
        total_packages: size.total_packages,
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
    use gradient_fixtures::mock::empty_db;

    #[tokio::test]
    async fn two_serves_accumulate_instead_of_writing() {
        let db = empty_db();
        let state = gradient_test_support::state::test_state(db);

        record_nar_traffic(&state.memory.cache_traffic, CacheId::nil(), 4096);
        record_nar_traffic(&state.memory.cache_traffic, CacheId::nil(), 1024);

        let taken = state.memory.cache_traffic.take();
        let bytes: i64 = taken.iter().map(|(_, _, t)| t.bytes).sum();
        let nars: i64 = taken.iter().map(|(_, _, t)| t.nars).sum();

        assert_eq!((bytes, nars), (5120, 2), "both serves counted: {taken:?}");
        assert!(
            taken.iter().all(|(_, bucket, _)| bucket.second() == 0),
            "a serve is bucketed by its minute: {taken:?}"
        );
        assert!(
            state.memory.cache_traffic.take().is_empty(),
            "a flush leaves nothing behind"
        );
    }
}
