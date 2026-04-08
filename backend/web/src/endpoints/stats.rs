/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{NaiveDateTime, Timelike, Utc};
use core::database::get_any_cache_by_name;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel,
    QueryFilter, Statement,
};
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
    let now = Utc::now().naive_utc();
    let bucket = match now.with_second(0).and_then(|t| t.with_nanosecond(0)) {
        Some(t) => t,
        None => now,
    };

    match ECacheMetric::find()
        .filter(CCacheMetric::Cache.eq(cache_id))
        .filter(CCacheMetric::BucketTime.eq(bucket))
        .one(&state.db)
        .await
    {
        Ok(Some(metric)) => {
            let mut am: ACacheMetric = metric.into_active_model();
            am.bytes_sent = Set(am.bytes_sent.unwrap() + bytes);
            am.nar_count = Set(am.nar_count.unwrap() + 1);
            if let Err(e) = am.update(&state.db).await {
                tracing::warn!(error = %e, "Failed to update cache metric");
            }
        }
        Ok(None) => {
            let am = ACacheMetric {
                id: Set(Uuid::new_v4()),
                cache: Set(cache_id),
                bucket_time: Set(bucket),
                bytes_sent: Set(bytes),
                nar_count: Set(1),
            };
            if let Err(e) = am.insert(&state.db).await {
                tracing::warn!(error = %e, "Failed to insert cache metric");
            }
        }
        Err(e) => tracing::warn!(error = %e, "Failed to query cache metric"),
    }
}

async fn aggregate_traffic(
    db: &sea_orm::DatabaseConnection,
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

async fn aggregate_storage(
    db: &sea_orm::DatabaseConnection,
    cache_id: Uuid,
    trunc_unit: &str,
    back_interval: &str,
) -> Result<Vec<StorageMetricPoint>, WebError> {
    let sql = format!(
        r#"SELECT gs.period,
                  COALESCE(COUNT(bos.id),      0)::bigint AS packages,
                  COALESCE(SUM(bo.file_size),  0)::bigint AS bytes
           FROM generate_series(
               date_trunc('{trunc_unit}', NOW() AT TIME ZONE 'UTC') - INTERVAL '{back_interval}',
               date_trunc('{trunc_unit}', NOW() AT TIME ZONE 'UTC'),
               INTERVAL '1 {trunc_unit}'
           ) AS gs(period)
           LEFT JOIN derivation_output_signature bos
               ON date_trunc('{trunc_unit}', bos.created_at) = gs.period
              AND bos.cache = $1
           LEFT JOIN derivation_output bo ON bo.id = bos.derivation_output
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
        .db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COALESCE(SUM(bo.file_size), 0)::bigint AS total_bytes,
                      COALESCE(SUM(bo.nar_size),  0)::bigint AS total_nar_bytes,
                      COUNT(bos.id)::bigint                   AS total_packages
               FROM derivation_output_signature bos
               JOIN derivation_output bo ON bo.id = bos.derivation_output
               WHERE bos.cache = $1"#,
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
            aggregate_storage(&state.db, cache.id, "minute", "59 minutes"),
            aggregate_storage(&state.db, cache.id, "hour", "23 hours"),
            aggregate_storage(&state.db, cache.id, "day", "29 days"),
            aggregate_storage(&state.db, cache.id, "week", "11 weeks"),
            aggregate_traffic(&state.db, cache.id, "minute", "59 minutes"),
            aggregate_traffic(&state.db, cache.id, "hour", "23 hours"),
            aggregate_traffic(&state.db, cache.id, "day", "29 days"),
            aggregate_traffic(&state.db, cache.id, "week", "11 weeks"),
        )?;

    Ok(Json(BaseResponse {
        error: false,
        message: CacheStatsResponse {
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
        },
    }))
}
