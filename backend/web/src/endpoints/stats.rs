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
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel,
    QueryFilter, Statement,
};
use sea_orm::ActiveValue::Set;
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
pub struct CacheStatsResponse {
    /// Total compressed bytes of all NARs cached by this cache.
    pub total_bytes: i64,
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
    interval: &str,
) -> Result<Vec<CacheMetricPoint>, WebError> {
    let sql = format!(
        r#"SELECT date_trunc('{trunc_unit}', bucket_time) as period,
                  COALESCE(SUM(bytes_sent), 0)::bigint as bytes,
                  COALESCE(SUM(nar_count), 0)::bigint as requests
           FROM cache_metric
           WHERE cache = $1
             AND bucket_time > (NOW() AT TIME ZONE 'UTC') - INTERVAL '{interval}'
           GROUP BY period
           ORDER BY period"#,
        trunc_unit = trunc_unit,
        interval = interval,
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

    // Total compressed bytes stored for this cache (sum of file_size of all signed outputs).
    let total_row = state
        .db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COALESCE(SUM(bo.file_size), 0)::bigint AS total
               FROM build_output bo
               JOIN build_output_signature bos ON bos.build_output = bo.id
               WHERE bos.cache = $1"#,
            [sea_orm::Value::Uuid(Some(Box::new(cache.id)))],
        ))
        .await
        .map_err(WebError::from)?;

    let total_bytes: i64 = total_row
        .as_ref()
        .and_then(|row| row.try_get::<i64>("", "total").ok())
        .unwrap_or(0);

    let (minutes, hours, days, weeks) = tokio::try_join!(
        aggregate_traffic(&state.db, cache.id, "minute", "60 minutes"),
        aggregate_traffic(&state.db, cache.id, "hour", "24 hours"),
        aggregate_traffic(&state.db, cache.id, "day", "30 days"),
        aggregate_traffic(&state.db, cache.id, "week", "12 weeks"),
    )?;

    Ok(Json(BaseResponse {
        error: false,
        message: CacheStatsResponse {
            total_bytes,
            minutes,
            hours,
            days,
            weeks,
        },
    }))
}
