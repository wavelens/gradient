/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Infrastructure board pages backed by aggregates rather than per-job rows:
//! Cache (traffic/storage), Network & API (NAR egress, worker speeds, HTTP),
//! Workers fleet time-series, and superuser System Health. Cache/NAR traffic is
//! shown as an anonymized infra aggregate; worker rows are org-scoped; HTTP and
//! process stats are superuser-only.

use crate::authorization::MaybeUser;
use crate::endpoints::metrics::{HttpRouteStat, ProcessStat, collect, http_snapshot, process_snapshot};
use crate::error::{WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_types::*;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct WindowParams {
    pub window_hours: Option<i64>,
}

fn window_clause(p: &WindowParams) -> i64 {
    p.window_hours.unwrap_or(24).clamp(1, 24 * 90)
}

#[derive(Serialize)]
pub struct SeriesPoint {
    pub bucket_start: String,
    pub count: i64,
    pub sum: f64,
}

/// Hourly rollup of `metric`, summed across every scope (anonymized infra view).
async fn infra_series(
    db: &impl ConnectionTrait,
    metric: &str,
    window_hours: i64,
) -> WebResult<Vec<SeriesPoint>> {
    let sql = format!(
        "SELECT bucket_start, sum(count)::bigint AS c, sum(sum) AS s \
         FROM metric_rollup WHERE metric = $1 AND granularity = 1 \
           AND bucket_start >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours' \
         GROUP BY bucket_start ORDER BY bucket_start"
    );

    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [metric.into()],
        ))
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let bucket: chrono::NaiveDateTime = r.try_get("", "bucket_start").unwrap_or_default();
            SeriesPoint {
                bucket_start: bucket.and_utc().to_rfc3339(),
                count: r.try_get("", "c").unwrap_or(0),
                sum: r.try_get("", "s").unwrap_or(0.0),
            }
        })
        .collect())
}

#[derive(Serialize)]
pub struct CacheTotals {
    pub bytes: i64,
    pub nar_bytes: i64,
    pub packages: i64,
    pub bytes_sent_total: i64,
    pub requests_total: i64,
}

#[derive(Serialize)]
pub struct BoardCacheStats {
    pub totals: CacheTotals,
    pub traffic: Vec<SeriesPoint>,
    pub storage: Vec<SeriesPoint>,
}

pub async fn get_board_cache(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<BoardCacheStats>>> {
    let window = window_clause(&params);
    let obs = collect(&state, &scheduler).await?;
    let traffic = infra_series(&state.web_db, "cache.bytes_sent", window).await?;
    let storage = infra_series(&state.web_db, "cache.bytes_added", window).await?;
    Ok(ok_json(BoardCacheStats {
        totals: CacheTotals {
            bytes: obs.cache_bytes,
            nar_bytes: obs.cache_nar_bytes,
            packages: obs.cache_packages,
            bytes_sent_total: obs.cache_nar_bytes_sent_total,
            requests_total: obs.cache_nar_requests_total,
        },
        traffic,
        storage,
    }))
}

#[derive(Serialize)]
pub struct WorkerNet {
    pub worker_id: Option<String>,
    pub network_speed_mbps: Option<f32>,
    pub disk_speed_mbps: Option<f32>,
}

#[derive(Serialize)]
pub struct BoardNetworkStats {
    pub nar_egress: Vec<SeriesPoint>,
    pub workers: Vec<WorkerNet>,
    /// Per-route HTTP latency/throughput; superuser-only, empty otherwise.
    pub http: Vec<HttpRouteStat>,
}

pub async fn get_board_network(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<BoardNetworkStats>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);
    let nar_egress = infra_series(&state.web_db, "cache.bytes_sent", window).await?;

    let mut sql = String::from(
        "SELECT DISTINCT ON (worker_id) worker_id, network_speed_mbps, disk_speed_mbps \
         FROM worker_sample \
         WHERE at >= (now() AT TIME ZONE 'UTC') - interval '1 hour'",
    );

    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(BoardNetworkStats { nar_egress, workers: vec![], http: vec![] }));
        }

        sql.push_str(&format!(" AND organization IN ({list})"));
    }

    sql.push_str(" ORDER BY worker_id, at DESC");
    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let workers = rows
        .into_iter()
        .map(|r| WorkerNet {
            worker_id: r.try_get("", "worker_id").ok(),
            network_speed_mbps: r.try_get("", "network_speed_mbps").ok().flatten(),
            disk_speed_mbps: r.try_get("", "disk_speed_mbps").ok().flatten(),
        })
        .collect();

    let http = if scope.is_all() { http_snapshot() } else { vec![] };
    Ok(ok_json(BoardNetworkStats { nar_egress, workers, http }))
}

#[derive(Serialize)]
pub struct BoardFleetPoint {
    pub bucket_start: String,
    pub connected: i64,
    pub draining: i64,
    pub eval: i64,
    pub fetch: i64,
    pub build: i64,
}

pub async fn get_board_fleet(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<Vec<BoardFleetPoint>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);
    let mut sql = format!(
        "SELECT date_trunc('hour', at) AS bucket, \
                count(DISTINCT worker_id) AS connected, \
                count(DISTINCT worker_id) FILTER (WHERE state = 1) AS draining, \
                count(DISTINCT worker_id) FILTER (WHERE (capabilities->>'eval')::boolean) AS ev, \
                count(DISTINCT worker_id) FILTER (WHERE (capabilities->>'fetch')::boolean) AS ft, \
                count(DISTINCT worker_id) FILTER (WHERE (capabilities->>'build')::boolean) AS bd \
         FROM worker_sample \
         WHERE at >= (now() AT TIME ZONE 'UTC') - interval '{window} hours'"
    );

    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(vec![]));
        }

        sql.push_str(&format!(" AND organization IN ({list})"));
    }

    sql.push_str(" GROUP BY bucket ORDER BY bucket");
    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let out = rows
        .into_iter()
        .map(|r| {
            let bucket: chrono::NaiveDateTime = r.try_get("", "bucket").unwrap_or_default();
            BoardFleetPoint {
                bucket_start: bucket.and_utc().to_rfc3339(),
                connected: r.try_get("", "connected").unwrap_or(0),
                draining: r.try_get("", "draining").unwrap_or(0),
                eval: r.try_get("", "ev").unwrap_or(0),
                fetch: r.try_get("", "ft").unwrap_or(0),
                build: r.try_get("", "bd").unwrap_or(0),
            }
        })
        .collect();

    Ok(ok_json(out))
}

const DURATION_BANDS: &[&str] = &["<10s", "10-30s", "30-60s", "1-3m", "3-10m", "10-30m", ">30m"];

#[derive(Serialize)]
pub struct HeatmapBand {
    pub band: &'static str,
    pub counts: Vec<i64>,
}

#[derive(Serialize)]
pub struct DurationsHeatmap {
    pub times: Vec<String>,
    pub bands: Vec<HeatmapBand>,
}

/// 2D build-duration distribution (duration band × hour) for the Durations page,
/// computed on demand from `build`. Org-scoped.
pub async fn get_board_durations_heatmap(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<DurationsHeatmap>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);
    let mut clauses = vec![
        "b.status = 3".to_string(),
        "b.build_time_ms IS NOT NULL".to_string(),
        format!("b.build_finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window} hours'"),
    ];

    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(DurationsHeatmap { times: vec![], bands: vec![] }));
        }

        clauses.push(format!("d.organization IN ({list})"));
    }

    let sql = format!(
        "SELECT date_trunc('hour', b.build_finished_at) AS t, \
                width_bucket(b.build_time_ms, ARRAY[10000,30000,60000,180000,600000,1800000]) AS band, \
                count(*)::bigint AS c \
         FROM build b JOIN derivation d ON d.id = b.derivation \
         WHERE {} GROUP BY t, band ORDER BY t",
        clauses.join(" AND ")
    );

    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let mut times: Vec<chrono::NaiveDateTime> = Vec::new();
    let mut cells: Vec<(chrono::NaiveDateTime, usize, i64)> = Vec::new();
    for r in &rows {
        let t: chrono::NaiveDateTime = r.try_get("", "t").unwrap_or_default();
        let band = (r.try_get::<i32>("", "band").unwrap_or(0) as usize).min(DURATION_BANDS.len() - 1);
        let c: i64 = r.try_get("", "c").unwrap_or(0);
        if !times.contains(&t) {
            times.push(t);
        }

        cells.push((t, band, c));
    }

    let bands = DURATION_BANDS
        .iter()
        .enumerate()
        .map(|(bi, label)| {
            let counts = times
                .iter()
                .map(|t| {
                    cells
                        .iter()
                        .find(|(ct, cb, _)| ct == t && *cb == bi)
                        .map(|(_, _, c)| *c)
                        .unwrap_or(0)
                })
                .collect();

            HeatmapBand { band: label, counts }
        })
        .collect();

    Ok(ok_json(DurationsHeatmap {
        times: times.into_iter().map(|t| t.and_utc().to_rfc3339()).collect(),
        bands,
    }))
}

#[derive(Serialize)]
pub struct BoardHealth {
    pub version: String,
    pub uptime_seconds: f64,
    pub workers_connected: i64,
    pub jobs_pending: i64,
    pub jobs_active: i64,
    pub cache_bytes: i64,
    pub cache_packages: i64,
    pub process: ProcessStat,
    pub http: Vec<HttpRouteStat>,
    pub rollup_lag_seconds: Option<f64>,
    pub latest_rollup_bucket: Option<String>,
}

pub async fn get_board_health(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<BoardHealth>>> {
    require_superuser(&user)?;
    let obs = collect(&state, &scheduler).await?;

    let latest: Option<chrono::NaiveDateTime> = state
        .web_db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT max(bucket_start) AS m FROM metric_rollup WHERE granularity = 0".to_owned(),
        ))
        .await?
        .and_then(|r| r.try_get("", "m").ok().flatten());

    let rollup_lag_seconds = latest.map(|t| (now() - t).num_milliseconds() as f64 / 1000.0);

    Ok(ok_json(BoardHealth {
        version: obs.version,
        uptime_seconds: obs.uptime_seconds,
        workers_connected: obs.workers_connected,
        jobs_pending: obs.jobs_pending,
        jobs_active: obs.jobs_active,
        cache_bytes: obs.cache_bytes,
        cache_packages: obs.cache_packages,
        process: process_snapshot(),
        http: http_snapshot(),
        rollup_lag_seconds,
        latest_rollup_bucket: latest.map(|t| t.and_utc().to_rfc3339()),
    }))
}
