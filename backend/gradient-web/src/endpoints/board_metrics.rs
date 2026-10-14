/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::metrics::{
    HttpRouteStat, Observations, ProcessStat, collect, http_snapshot, process_snapshot,
};
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::metrics::worker_samples::{self, WorkerNet};
use gradient_db::metrics::{build_times, series};
use gradient_scheduler::Scheduler;
use gradient_types::*;
use gradient_util::telemetry::metric;
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WindowParams {
    pub window_hours: Option<i64>,
}

fn window_clause(p: &WindowParams) -> i64 {
    p.window_hours.unwrap_or(24).clamp(1, 24 * 90)
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SeriesPoint {
    pub bucket_start: String,
    pub count: i64,
    pub sum: f64,
}

impl From<series::BucketTotal> for SeriesPoint {
    fn from(total: series::BucketTotal) -> Self {
        Self {
            bucket_start: total.bucket_start.and_utc().to_rfc3339(),
            count: total.count,
            sum: total.sum,
        }
    }
}

async fn infra_series(
    db: &impl ConnectionTrait,
    metric: &str,
    window_hours: i64,
) -> WebResult<Vec<SeriesPoint>> {
    let rows = series::hourly_totals(db, metric, window_hours).await?;

    Ok(rows.into_iter().map(SeriesPoint::from).collect())
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CacheTotals {
    pub bytes: i64,
    pub nar_bytes: i64,
    pub packages: i64,
    pub bytes_sent_total: i64,
    pub requests_total: i64,
}

impl From<&Observations> for CacheTotals {
    fn from(obs: &Observations) -> Self {
        Self {
            bytes: obs.cache_bytes,
            nar_bytes: obs.cache_nar_bytes,
            packages: obs.cache_packages,
            bytes_sent_total: obs.cache_nar_bytes_sent_total,
            requests_total: obs.cache_nar_requests_total,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BoardCacheStats {
    pub totals: CacheTotals,
    pub traffic: Vec<SeriesPoint>,
    pub storage: Vec<SeriesPoint>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BoardUpstreamCache {
    pub upstream_id: String,
    pub display_name: String,
    pub url: String,
    pub avg_latency_ms: Option<f64>,
    pub hit_rate: Option<f64>,
    pub requests_total: i64,
    pub latency: Vec<SeriesPoint>,
    pub hit_rate_series: Vec<SeriesPoint>,
}

#[derive(Serialize, Default, utoipa::ToSchema)]
pub struct BoardUpstreamCacheStats {
    pub upstream_caches: Vec<BoardUpstreamCache>,
}

#[utoipa::path(
    get,
    path = "/board/cache",
    operation_id = "getBoardCache",
    tag = "board",
    summary = "Cache statistics",
    description = "Cache totals plus hourly traffic and storage-growth series, aggregated across caches (anonymized infra view).",
    params(WindowParams),
    responses((status = OK, description = "Cache statistics", body = BoardCacheStats), WebError)
)]
pub async fn get_board_cache(
    State(state): State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<BoardCacheStats>>> {
    let window = window_clause(&params);
    let obs = collect(
        &state.web_db,
        state.started_at,
        &state.memory.upload_admission,
        &scheduler,
    )
    .await?;
    let traffic = infra_series(&state.web_db, metric::CACHE_BYTES_SENT, window).await?;
    let storage = infra_series(&state.web_db, metric::CACHE_BYTES_ADDED, window).await?;
    Ok(ok_json(BoardCacheStats {
        totals: CacheTotals::from(&obs),
        traffic,
        storage,
    }))
}

fn upstream_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = after_scheme.split('/').next().unwrap_or(after_scheme);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

#[utoipa::path(
    get,
    path = "/board/cache/upstream-caches",
    operation_id = "getBoardUpstreamCaches",
    tag = "board",
    summary = "Per-URL cache latency and narinfo hit-rate",
    description = "Per-upstream-URL trailing latency, narinfo hit-rate, and request counts with hourly series. Metrics are per URL: the same URL registered under several caches/projects contributes to one series; `display_name` is the URL host. Project-scoped: superusers see all URLs; members see the URLs used by their projects plus public projects. Each series' `url` is the caller's own upstream, so it is returned unmasked. The entries are returned as `upstream_caches`.",
    params(WindowParams),
    responses((status = OK, description = "Per-upstream cache performance", body = BoardUpstreamCacheStats), WebError)
)]
pub async fn get_board_upstream_caches(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<BoardUpstreamCacheStats>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);

    let rows = series::upstream_cache_buckets(&state.web_db, window).await?;

    use std::collections::HashMap;
    struct Agg {
        latency: Vec<SeriesPoint>,
        hits: HashMap<String, f64>,
        misses: HashMap<String, f64>,
        buckets: Vec<String>,
        latency_sum: f64,
        latency_count: i64,
        hits_total: i64,
        misses_total: i64,
    }

    let mut by_upstream: HashMap<String, Agg> = HashMap::new();
    for r in rows {
        let Some(uid) = r.upstream_url else {
            continue;
        };
        let metric = r.metric;
        let bucket_s = r.bucket_start.and_utc().to_rfc3339();
        let c = r.count;
        let s = r.sum;

        let agg = by_upstream.entry(uid).or_insert_with(|| Agg {
            latency: Vec::new(),
            hits: HashMap::new(),
            misses: HashMap::new(),
            buckets: Vec::new(),
            latency_sum: 0.0,
            latency_count: 0,
            hits_total: 0,
            misses_total: 0,
        });

        match metric.as_str() {
            metric::UPSTREAM_LATENCY_MS => {
                let avg = if c > 0 { s / c as f64 } else { 0.0 };
                agg.latency.push(SeriesPoint {
                    bucket_start: bucket_s.clone(),
                    count: c,
                    sum: avg,
                });
                if !agg.buckets.contains(&bucket_s) {
                    agg.buckets.push(bucket_s);
                }

                agg.latency_sum += s;
                agg.latency_count += c;
            }
            #[expect(clippy::cast_possible_truncation, reason = "counts are whole numbers")]
            metric::UPSTREAM_NARINFO_HITS => {
                agg.hits.insert(bucket_s, s);
                agg.hits_total += s.round() as i64;
            }
            #[expect(clippy::cast_possible_truncation, reason = "counts are whole numbers")]
            metric::UPSTREAM_NARINFO_MISSES => {
                agg.misses.insert(bucket_s, s);
                agg.misses_total += s.round() as i64;
            }
            _ => {}
        }
    }

    if let Some(list) = scope.project_in_list() {
        if list.is_empty() {
            return Ok(ok_json(BoardUpstreamCacheStats::default()));
        }

        let allowed =
            gradient_db::caches::upstream::upstream_urls_for_projects(&state.web_db, &list).await?;
        by_upstream.retain(|url, _| allowed.contains(url));
    }

    let mut upstream_caches = Vec::new();
    for (uid, agg) in by_upstream {
        let hit_rate_series = agg
            .buckets
            .iter()
            .map(|b| {
                let h = *agg.hits.get(b).unwrap_or(&0.0);
                let m = *agg.misses.get(b).unwrap_or(&0.0);
                let denom = h + m;
                SeriesPoint {
                    bucket_start: b.clone(),
                    count: 0,
                    sum: if denom > 0.0 { h / denom } else { 0.0 },
                }
            })
            .collect();

        let avg_latency_ms = if agg.latency_count > 0 {
            Some(agg.latency_sum / agg.latency_count as f64)
        } else {
            None
        };
        let denom = (agg.hits_total + agg.misses_total) as f64;
        let hit_rate = if denom > 0.0 {
            Some(agg.hits_total as f64 / denom)
        } else {
            None
        };
        let display_name = upstream_host(&uid);

        upstream_caches.push(BoardUpstreamCache {
            upstream_id: uid.clone(),
            display_name,
            url: uid,
            avg_latency_ms,
            hit_rate,
            requests_total: agg.latency_count,
            latency: agg.latency,
            hit_rate_series,
        });
    }

    upstream_caches.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(ok_json(BoardUpstreamCacheStats { upstream_caches }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BoardNetworkStats {
    pub nar_egress: Vec<SeriesPoint>,
    pub workers: Vec<WorkerNet>,
    pub http: Vec<HttpRouteStat>,
}

#[utoipa::path(
    get,
    path = "/board/network",
    operation_id = "getBoardNetwork",
    tag = "board",
    summary = "Network & API statistics",
    description = "NAR egress series, per-worker latest upload, download and disk speeds with the worker's `worker_name` (project-scoped), and per-route HTTP latency/throughput (superuser-only).",
    params(WindowParams),
    responses((status = OK, description = "Network statistics", body = BoardNetworkStats), WebError)
)]
pub async fn get_board_network(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<BoardNetworkStats>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);
    let nar_egress = infra_series(&state.web_db, metric::CACHE_BYTES_SENT, window).await?;

    let project_filter = scope.project_in_list();
    let workers = match &project_filter {
        Some(list) if list.is_empty() => vec![],
        _ => worker_samples::latest_speeds(&state.web_db, project_filter.as_deref()).await?,
    };

    let http = if scope.is_all() {
        http_snapshot()
    } else {
        vec![]
    };
    Ok(ok_json(BoardNetworkStats {
        nar_egress,
        workers,
        http,
    }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BoardFleetPoint {
    pub bucket_start: String,
    pub connected: i64,
    pub draining: i64,
    pub eval: i64,
    pub fetch: i64,
    pub build: i64,
}

impl From<worker_samples::FleetCount> for BoardFleetPoint {
    fn from(fleet: worker_samples::FleetCount) -> Self {
        Self {
            bucket_start: fleet.hour.and_utc().to_rfc3339(),
            connected: fleet.connected,
            draining: fleet.draining,
            eval: fleet.eval,
            fetch: fleet.fetch,
            build: fleet.build,
        }
    }
}

#[utoipa::path(
    get,
    path = "/board/fleet",
    operation_id = "getBoardFleet",
    tag = "board",
    summary = "Worker fleet time-series",
    description = "Hourly connected / draining / by-capability worker counts derived from worker samples, scoped to the caller's projects.",
    params(WindowParams),
    responses((status = OK, description = "Fleet time-series", body = Vec<BoardFleetPoint>), WebError)
)]
pub async fn get_board_fleet(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<Vec<BoardFleetPoint>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);

    let project_filter = scope.project_in_list();
    if let Some(list) = &project_filter
        && list.is_empty()
    {
        return Ok(ok_json(vec![]));
    }

    let out = worker_samples::fleet_per_hour(&state.web_db, window, project_filter.as_deref())
        .await?
        .into_iter()
        .map(BoardFleetPoint::from)
        .collect();

    Ok(ok_json(out))
}

const DURATION_BANDS: &[&str] = &[
    "<10s", "10-30s", "30-60s", "1-3m", "3-10m", "10-30m", ">30m",
];

#[derive(Serialize, utoipa::ToSchema)]
pub struct HeatmapBand {
    pub band: &'static str,
    pub counts: Vec<i64>,
}

#[derive(Serialize, Default, utoipa::ToSchema)]
pub struct DurationsHeatmap {
    pub times: Vec<String>,
    pub bands: Vec<HeatmapBand>,
}

#[utoipa::path(
    get,
    path = "/board/durations/heatmap",
    operation_id = "getBoardDurationsHeatmap",
    tag = "board",
    summary = "Build-duration heatmap",
    description = "2D build-duration distribution (duration band × hour) computed on demand from builds, scoped to the caller's projects.",
    params(WindowParams),
    responses((status = OK, description = "Duration heatmap", body = DurationsHeatmap), WebError)
)]
pub async fn get_board_durations_heatmap(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<WindowParams>,
) -> WebResult<Json<BaseResponse<DurationsHeatmap>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = window_clause(&params);

    let project_filter = scope.project_in_list();
    if let Some(list) = &project_filter
        && list.is_empty()
    {
        return Ok(ok_json(DurationsHeatmap::default()));
    }

    let rows =
        build_times::durations_per_hour(&state.web_db, window, project_filter.as_deref()).await?;

    let mut times: Vec<chrono::NaiveDateTime> = Vec::new();
    let mut cells: Vec<(chrono::NaiveDateTime, usize, i64)> = Vec::new();
    for r in &rows {
        let t = r.hour;
        let band = (r.band as usize).min(DURATION_BANDS.len() - 1);
        let c = r.count;
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

            HeatmapBand {
                band: label,
                counts,
            }
        })
        .collect();

    Ok(ok_json(DurationsHeatmap {
        times: times
            .into_iter()
            .map(|t| t.and_utc().to_rfc3339())
            .collect(),
        bands,
    }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SupervisedLoop {
    pub name: String,
    pub restarts: u32,
    pub pass_errors: u64,
    pub pass_timeouts: u64,
    pub last_ok_seconds_ago: Option<f64>,
    pub last_error: Option<String>,
}

fn loops_view(
    rows: Vec<(&'static str, gradient_util::supervision::LoopHealth)>,
    now: std::time::Instant,
) -> Vec<SupervisedLoop> {
    rows.into_iter()
        .map(|(name, h)| SupervisedLoop {
            name: name.to_string(),
            restarts: h.restarts,
            pass_errors: h.pass_errors,
            pass_timeouts: h.pass_timeouts,
            last_ok_seconds_ago: h.last_ok_at.map(|t| now.duration_since(t).as_secs_f64()),
            last_error: h.last_error,
        })
        .collect()
}

#[derive(Serialize, utoipa::ToSchema)]
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
    pub draining: bool,
    pub supervised: Vec<SupervisedLoop>,
    pub proto_sessions: usize,
    pub unconfirmed_nars: u64,
    /// A rising `failed_deliveries` points at a Git host or mail host that is down, not at a
    /// backlog. Those rows stay dead letters until an operator acts.
    pub pending_deliveries: i64,
    pub failed_deliveries: i64,
    pub hot_nar_cache: HotNarCacheHealth,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HotNarCacheHealth {
    pub entries: u64,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

impl From<gradient_storage::HotNarStats> for HotNarCacheHealth {
    fn from(s: gradient_storage::HotNarStats) -> Self {
        Self {
            entries: s.entries,
            bytes: s.bytes,
            hits: s.hits,
            misses: s.misses,
            evictions: s.evictions,
        }
    }
}

#[utoipa::path(
    get,
    path = "/board/health",
    operation_id = "getBoardHealth",
    tag = "board",
    summary = "System health (superuser)",
    description = "Process/runtime snapshot, pipeline rollup lag, HTTP route stats, and the instance `draining` flag. Superuser only.",
    responses((status = OK, description = "System health", body = BoardHealth), WebError)
)]
pub async fn get_board_health(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Extension(limiter): Extension<Arc<gradient_wire::ProtoLimiter>>,
) -> WebResult<Json<BaseResponse<BoardHealth>>> {
    require_superuser(&user)?;
    let obs = collect(
        &state.web_db,
        state.started_at,
        &state.memory.upload_admission,
        &scheduler,
    )
    .await?;

    let latest = series::latest_minute_bucket(&state.web_db).await?;

    let rollup_lag_seconds = latest.map(|t| (now() - t).num_milliseconds() as f64 / 1000.0);
    let (pending_deliveries, failed_deliveries) =
        gradient_db::deliveries::pending::pending_counts(&state.web_db).await?;

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
        draining: scheduler.draining.is_draining().await,
        supervised: loops_view(scheduler.loop_health(), std::time::Instant::now()),
        proto_sessions: limiter.in_use(),
        unconfirmed_nars: gradient_db::caches::capacity::unconfirmed_cached_path_count(
            &state.web_db,
        )
        .await?,
        pending_deliveries,
        failed_deliveries,
        hot_nar_cache: state.nar_storage.hot().stats().into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::upstream_host;

    #[test]
    fn upstream_host_strips_scheme_and_path() {
        assert_eq!(
            upstream_host("https://cache.nixos.org/foo?x=1"),
            "cache.nixos.org"
        );
        assert_eq!(upstream_host("http://10.0.0.1:5000/"), "10.0.0.1:5000");
        assert_eq!(upstream_host("cache.example.com"), "cache.example.com");
    }

    #[test]
    fn loops_view_reports_age_of_last_ok_pass() {
        use gradient_util::supervision::LoopHealth;
        let now = std::time::Instant::now();
        let rows = vec![(
            "build-dispatch",
            LoopHealth {
                restarts: 1,
                pass_errors: 2,
                pass_timeouts: 0,
                last_ok_at: Some(now - std::time::Duration::from_secs(5)),
                last_error: Some("boom".into()),
            },
        )];
        let view = super::loops_view(rows, now);
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].name, "build-dispatch");
        assert_eq!(view[0].restarts, 1);
        assert_eq!(view[0].last_ok_seconds_ago, Some(5.0));
        assert_eq!(view[0].last_error.as_deref(), Some("boom"));
    }
}
