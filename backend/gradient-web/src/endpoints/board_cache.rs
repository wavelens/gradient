/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::board_cache_condition;
use crate::authorization::MaybeUser;
use crate::endpoints::board_metrics::{SeriesPoint, WindowParams, infra_series, window_clause};
use crate::endpoints::metrics::{Observations, collect};
use crate::endpoints::stats::{TrafficKind, TrafficParams};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use chrono::{DateTime, SecondsFormat, Utc};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::metrics::series;
use gradient_scheduler::Scheduler;
use gradient_storage::admission::UploadAdmission;
use gradient_types::events::{Event, EventRx};
use gradient_types::*;
use gradient_util::shutdown::CancellationToken;
use gradient_util::telemetry::metric;
use sea_orm::Condition;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const SNAPSHOT_EVENT: &str = "board.cache";
const MIN_SNAPSHOT_GAP: Duration = Duration::from_secs(2);
const BUSIEST_CACHES: usize = 10;

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

#[derive(Serialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct BoardCacheTraffic {
    pub name: String,
    pub display_name: String,
    pub bytes: i64,
    pub requests: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BoardCacheStats {
    pub totals: CacheTotals,
    pub traffic: Vec<SeriesPoint>,
    pub storage: Vec<SeriesPoint>,
    pub caches: Vec<BoardCacheTraffic>,
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct BoardCacheSnapshot {
    pub stats: BoardCacheStats,
    pub upstream: BoardUpstreamCacheStats,
}

#[utoipa::path(
    get,
    path = "/board/cache",
    operation_id = "getBoardCache",
    tag = "board",
    summary = "Cache statistics",
    description = "Cache totals plus hourly traffic and storage-growth series, aggregated across caches (anonymized infra view). `caches` lists the 10 caches with the most bytes served in the window, largest first, limited to public caches and the caches the caller can see (all caches for superusers).",
    params(WindowParams, TrafficParams),
    responses((status = OK, description = "Cache statistics", body = BoardCacheStats), WebError)
)]
pub async fn get_board_cache(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Query(params): Query<WindowParams>,
    Query(TrafficParams { traffic }): Query<TrafficParams>,
) -> WebResult<Json<BaseResponse<BoardCacheStats>>> {
    let obs = collect(
        &state.web_db,
        state.started_at,
        &state.memory.upload_admission,
        &scheduler,
    )
    .await?;
    let visible = board_cache_condition(&state.web_db, &maybe_user).await?;
    let stats = cache_stats(
        &state.web_db,
        &obs,
        window_clause(&params),
        traffic,
        &visible,
    )
    .await?;
    Ok(ok_json(stats))
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
    let upstream = upstream_caches(&state.web_db, &scope, window_clause(&params)).await?;
    Ok(ok_json(upstream))
}

#[utoipa::path(
    get,
    path = "/board/cache/live",
    operation_id = "boardCacheLive",
    tag = "board",
    summary = "Cache statistics live updates (WebSocket)",
    description = "Upgrades to a WebSocket that pushes a `board.cache` event whose `content` is a `BoardCacheSnapshot`: the `GET /board/cache` view as `stats` and the `GET /board/cache/upstream-caches` view as `upstream`, scoped like those endpoints. The first frame follows the upgrade. Later frames follow cache changes, at most one per 2 seconds, and only when the snapshot differs from the last one sent.",
    params(WindowParams, TrafficParams),
    responses((status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"), WebError)
)]
pub async fn cache_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Query(params): Query<WindowParams>,
    Query(TrafficParams { traffic }): Query<TrafficParams>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let feed = CacheFeed {
        db: state.web_db.clone(),
        started_at: state.started_at,
        admission: state.memory.upload_admission.clone(),
        scheduler,
        scope: MetricsScope::resolve(&state.web_db, &maybe_user).await?,
        visible: board_cache_condition(&state.web_db, &maybe_user).await?,
        window: window_clause(&params),
        traffic,
    };
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(stream_snapshots(socket, rx, feed, cancel))
            .await;
    }))
}

async fn cache_stats(
    db: &WebDb,
    obs: &Observations,
    window: i64,
    traffic: TrafficKind,
    visible: &Condition,
) -> WebResult<BoardCacheStats> {
    Ok(BoardCacheStats {
        totals: CacheTotals::from(obs),
        traffic: infra_series(db, traffic.metric(), window).await?,
        storage: infra_series(db, metric::CACHE_BYTES_ADDED, window).await?,
        caches: busiest_caches(db, window, traffic, visible).await?,
    })
}

async fn busiest_caches(
    db: &WebDb,
    window: i64,
    traffic: TrafficKind,
    visible: &Condition,
) -> WebResult<Vec<BoardCacheTraffic>> {
    let totals = series::traffic_per_cache(db, traffic.metric(), window).await?;
    let ids: Vec<CacheId> = totals.iter().map(|t| t.cache).collect();
    let caches = gradient_db::caches::among_matching(db, &ids, visible.clone()).await?;
    Ok(rank_caches(totals, caches))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "byte sums are whole numbers"
)]
fn rank_caches(
    totals: Vec<series::CacheTrafficTotal>,
    caches: Vec<MCache>,
) -> Vec<BoardCacheTraffic> {
    let caches: HashMap<CacheId, MCache> = caches.into_iter().map(|c| (c.id, c)).collect();
    let mut ranked: Vec<BoardCacheTraffic> = totals
        .into_iter()
        .filter_map(|t| {
            let cache = caches.get(&t.cache)?;
            Some(BoardCacheTraffic {
                name: cache.name.clone(),
                display_name: cache.display_name.clone(),
                bytes: t.bytes.round() as i64,
                requests: t.requests,
            })
        })
        .collect();
    ranked.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    ranked.truncate(BUSIEST_CACHES);
    ranked
}

#[derive(Default)]
struct UpstreamTally {
    latency: Vec<SeriesPoint>,
    hits: HashMap<String, f64>,
    misses: HashMap<String, f64>,
    buckets: Vec<String>,
    latency_sum: f64,
    latency_count: i64,
    hits_total: i64,
    misses_total: i64,
}

impl UpstreamTally {
    fn add(&mut self, row: series::UpstreamCacheBucket) {
        let bucket = row.bucket_start.and_utc().to_rfc3339();
        match row.metric.as_str() {
            metric::UPSTREAM_LATENCY_MS => {
                let avg = if row.count > 0 {
                    row.sum / row.count as f64
                } else {
                    0.0
                };
                self.latency.push(SeriesPoint {
                    bucket_start: bucket.clone(),
                    count: row.count,
                    sum: avg,
                });
                if !self.buckets.contains(&bucket) {
                    self.buckets.push(bucket);
                }
                self.latency_sum += row.sum;
                self.latency_count += row.count;
            }
            #[expect(clippy::cast_possible_truncation, reason = "counts are whole numbers")]
            metric::UPSTREAM_NARINFO_HITS => {
                self.hits_total += row.sum.round() as i64;
                self.hits.insert(bucket, row.sum);
            }
            #[expect(clippy::cast_possible_truncation, reason = "counts are whole numbers")]
            metric::UPSTREAM_NARINFO_MISSES => {
                self.misses_total += row.sum.round() as i64;
                self.misses.insert(bucket, row.sum);
            }
            _ => {}
        }
    }

    fn into_upstream(self, url: String) -> BoardUpstreamCache {
        let hit_rate_series = self
            .buckets
            .iter()
            .map(|b| {
                let h = *self.hits.get(b).unwrap_or(&0.0);
                let m = *self.misses.get(b).unwrap_or(&0.0);
                let denom = h + m;
                SeriesPoint {
                    bucket_start: b.clone(),
                    count: 0,
                    sum: if denom > 0.0 { h / denom } else { 0.0 },
                }
            })
            .collect();
        let avg_latency_ms =
            (self.latency_count > 0).then(|| self.latency_sum / self.latency_count as f64);
        let lookups = (self.hits_total + self.misses_total) as f64;
        let hit_rate = (lookups > 0.0).then(|| self.hits_total as f64 / lookups);

        BoardUpstreamCache {
            upstream_id: url.clone(),
            display_name: upstream_host(&url),
            url,
            avg_latency_ms,
            hit_rate,
            requests_total: self.latency_count,
            latency: self.latency,
            hit_rate_series,
        }
    }
}

async fn upstream_caches(
    db: &WebDb,
    scope: &MetricsScope,
    window: i64,
) -> WebResult<BoardUpstreamCacheStats> {
    let mut by_upstream: HashMap<String, UpstreamTally> = HashMap::new();
    for row in series::upstream_cache_buckets(db, window).await? {
        if let Some(url) = row.upstream_url.clone() {
            by_upstream.entry(url).or_default().add(row);
        }
    }

    if let Some(list) = scope.project_in_list() {
        if list.is_empty() {
            return Ok(BoardUpstreamCacheStats::default());
        }
        let allowed = gradient_db::caches::upstream::upstream_urls_for_projects(db, &list).await?;
        by_upstream.retain(|url, _| allowed.contains(url));
    }

    let mut upstream_caches: Vec<BoardUpstreamCache> = by_upstream
        .into_iter()
        .map(|(url, tally)| tally.into_upstream(url))
        .collect();
    upstream_caches.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(BoardUpstreamCacheStats { upstream_caches })
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

struct CacheFeed {
    db: WebDb,
    started_at: DateTime<Utc>,
    admission: Arc<UploadAdmission>,
    scheduler: Arc<Scheduler>,
    scope: MetricsScope,
    visible: Condition,
    window: i64,
    traffic: TrafficKind,
}

impl CacheFeed {
    async fn snapshot(&self) -> WebResult<Value> {
        let obs = collect(&self.db, self.started_at, &self.admission, &self.scheduler).await?;
        let stats = cache_stats(&self.db, &obs, self.window, self.traffic, &self.visible).await?;
        let snapshot = BoardCacheSnapshot {
            stats,
            upstream: upstream_caches(&self.db, &self.scope, self.window).await?,
        };
        Ok(serde_json::to_value(snapshot).unwrap_or_default())
    }
}

/// Holds back a snapshot equal to the last one sent, so an unchanged view never redraws the
/// charts of every open board.
fn changed_frame(last: &mut Option<Value>, content: Value, at: DateTime<Utc>) -> Option<String> {
    if last.as_ref() == Some(&content) {
        return None;
    }
    let line = serde_json::json!({
        "event": SNAPSHOT_EVENT,
        "at": at.to_rfc3339_opts(SecondsFormat::AutoSi, true),
        "content": content,
    })
    .to_string();
    *last = Some(content);
    Some(line)
}

async fn stream_snapshots(
    socket: WebSocket,
    mut rx: EventRx,
    feed: CacheFeed,
    cancel: CancellationToken,
) {
    use futures::{SinkExt, StreamExt};
    use tokio::sync::broadcast::error::RecvError;

    let (mut sink, mut stream) = socket.split();
    let mut gap = tokio::time::interval(MIN_SNAPSHOT_GAP);
    gap.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stale = true;
    let mut last = None;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            incoming = stream.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
            event = rx.recv() => match event {
                Ok(env) => stale |= matches!(env.event, Event::CacheChanged(_)),
                Err(RecvError::Lagged(_)) => stale = true,
                Err(RecvError::Closed) => break,
            },
            _ = gap.tick(), if stale => {
                stale = false;
                let Ok(content) = feed.snapshot().await else { continue };
                if let Some(line) = changed_frame(&mut last, content, Utc::now())
                    && sink.send(Message::Text(line.into())).await.is_err()
                {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn busiest_caches_rank_largest_first_and_drop_caches_the_viewer_cannot_see() {
        use gradient_fixtures::rows::cache_with_id;
        let id = |n| CacheId::new(uuid::Uuid::from_u128(n));
        let owner = UserId::new(uuid::Uuid::from_u128(99));
        let total = |n, bytes| series::CacheTrafficTotal {
            cache: id(n),
            requests: 3,
            bytes,
        };
        let totals = (1..=12)
            .map(|n| total(n, n as f64 * 100.0))
            .chain([total(13, 1e9)])
            .collect();
        let visible = (1..=12)
            .map(|n| cache_with_id(id(n), &format!("c{n:02}"), owner))
            .collect();

        let ranked = rank_caches(totals, visible);

        let names: Vec<&str> = ranked.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "c12", "c11", "c10", "c09", "c08", "c07", "c06", "c05", "c04", "c03"
            ]
        );
        assert_eq!(ranked[0].bytes, 1200);
    }

    #[test]
    fn an_unchanged_snapshot_is_held_back_and_a_changed_one_is_framed() {
        let mut last = None;
        let at = Utc::now();
        let first = changed_frame(&mut last, serde_json::json!({ "n": 1 }), at)
            .expect("the first snapshot is always sent");
        let frame: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(frame["event"], "board.cache");
        assert_eq!(frame["content"], serde_json::json!({ "n": 1 }));

        assert!(changed_frame(&mut last, serde_json::json!({ "n": 1 }), at).is_none());
        assert!(changed_frame(&mut last, serde_json::json!({ "n": 2 }), at).is_some());
    }
}
