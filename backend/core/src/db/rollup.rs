/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background aggregator that folds fact tables into `metric_rollup`.
//!
//! Each pass recomputes minute buckets for a trailing window from the fact
//! tables (idempotent via `ON CONFLICT`), then cascades minute→hour→day→week
//! over `metric_rollup` itself. Best-effort: SQL failures are logged, never
//! propagated. Timestamps are compared in UTC to match the naive-UTC values the
//! recording layer writes via `crate::types::now()`.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::ConnectionTrait;
use tracing::{debug, warn};

use crate::types::ServerState;

/// A simple count metric over the `build` table, attributed to the owning org
/// via the `derivation` join.
struct BuildCount {
    name: &'static str,
    time_col: &'static str,
    filter: &'static str,
}

const BUILD_COUNTS: &[BuildCount] = &[
    BuildCount { name: "builds.created", time_col: "created_at", filter: "TRUE" },
    BuildCount { name: "builds.dispatched", time_col: "dispatched_at", filter: "TRUE" },
    BuildCount { name: "builds.completed", time_col: "build_finished_at", filter: "b.status = 3" },
    BuildCount { name: "builds.substituted", time_col: "build_finished_at", filter: "b.status = 7" },
    BuildCount {
        name: "builds.failed",
        time_col: "build_finished_at",
        filter: "b.status IN (4, 6, 8, 9)",
    },
];

/// A duration metric over the `build` table: milliseconds between two columns.
struct BuildDuration {
    name: &'static str,
    start_col: &'static str,
    end_col: &'static str,
    filter: &'static str,
}

const BUILD_DURATIONS: &[BuildDuration] = &[
    BuildDuration {
        name: "builds.duration_ms",
        start_col: "build_started_at",
        end_col: "build_finished_at",
        filter: "b.status = 3",
    },
    // Queue wait excluding dependency wait: ready (deps satisfied) → dispatched.
    BuildDuration {
        name: "dispatch.wait_ms",
        start_col: "ready_at",
        end_col: "dispatched_at",
        filter: "TRUE",
    },
    // Dependency wait: entered the queue → all dependencies satisfied.
    BuildDuration {
        name: "deps.wait_ms",
        start_col: "queued_at",
        end_col: "ready_at",
        filter: "TRUE",
    },
];

/// A count metric over `evaluation`, attributed to the org via the project join.
struct EvalCount {
    name: &'static str,
    filter: &'static str,
}

const EVAL_COUNTS: &[EvalCount] = &[
    EvalCount { name: "evals.completed", filter: "e.status = 5" },
    EvalCount { name: "evals.failed", filter: "e.status IN (6, 7)" },
];

/// (target_granularity, source_granularity, date_trunc unit, trailing window).
const CASCADES: &[(i16, i16, &str, &str)] = &[
    (1, 0, "hour", "3 hours"),
    (2, 1, "day", "2 days"),
    (3, 2, "week", "2 weeks"),
];

const MINUTE_WINDOW: &str = "15 minutes";

/// Cache traffic per minute per cache (scope `{cache}`): `count` = requests,
/// `sum` = bytes served. Source is the already-minute-bucketed `cache_metric`.
const CACHE_TRAFFIC_SQL: &str = "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT gen_random_uuid(), 'cache.bytes_sent', 0, cm.bucket_time, \
           jsonb_build_object('cache', cm.cache::text), hashtextextended(cm.cache::text, 0), \
           sum(cm.nar_count)::bigint, sum(cm.bytes_sent), \
           min(cm.bytes_sent), max(cm.bytes_sent), sum(power(cm.bytes_sent, 2)), NULL \
    FROM cache_metric cm \
    WHERE cm.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
    GROUP BY cm.bucket_time, cm.cache \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET count = EXCLUDED.count, sum = EXCLUDED.sum, \
                  min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq";

/// Cache storage added per minute per cache (scope `{cache}`): `count` =
/// packages added, `sum` = compressed bytes added.
const CACHE_STORAGE_SQL: &str = "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT gen_random_uuid(), 'cache.bytes_added', 0, date_trunc('minute', cps.created_at), \
           jsonb_build_object('cache', cps.cache::text), hashtextextended(cps.cache::text, 0), \
           count(*)::bigint, sum(coalesce(cp.file_size, 0)), 0, 0, 0, NULL \
    FROM cached_path_signature cps JOIN cached_path cp ON cp.id = cps.cached_path \
    WHERE cps.created_at >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
    GROUP BY date_trunc('minute', cps.created_at), cps.cache \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET count = EXCLUDED.count, sum = EXCLUDED.sum";

pub fn start_rollup_loop(state: Arc<ServerState>) {
    let shutdown = state.shutdown.clone();
    shutdown.spawn(async move { rollup_loop(state).await });
}

async fn rollup_loop(state: Arc<ServerState>) {
    let secs = state.config.metrics_args.metrics_rollup_interval_secs.max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        run_rollup(&state).await;
    }
}

async fn run_rollup(state: &Arc<ServerState>) {
    let db = &state.worker_db;
    for m in BUILD_COUNTS {
        if let Err(e) = db.execute_unprepared(&build_count_sql(m)).await {
            warn!(metric = m.name, error = %e, "rollup build-count failed");
        }
    }
    for m in BUILD_DURATIONS {
        if let Err(e) = db.execute_unprepared(&build_duration_sql(m)).await {
            warn!(metric = m.name, error = %e, "rollup build-duration failed");
        }
    }
    for m in EVAL_COUNTS {
        if let Err(e) = db.execute_unprepared(&eval_count_sql(m)).await {
            warn!(metric = m.name, error = %e, "rollup eval-count failed");
        }
    }
    if let Err(e) = db.execute_unprepared(CACHE_TRAFFIC_SQL).await {
        warn!(error = %e, "rollup cache-traffic failed");
    }
    if let Err(e) = db.execute_unprepared(CACHE_STORAGE_SQL).await {
        warn!(error = %e, "rollup cache-storage failed");
    }
    for (target, source, unit, window) in CASCADES {
        if let Err(e) = db.execute_unprepared(&cascade_sql(*target, *source, unit, window)).await {
            warn!(target, error = %e, "rollup cascade failed");
        }
    }
    debug!("rollup pass complete");
}

fn build_count_sql(m: &BuildCount) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT gen_random_uuid(), '{name}', 0, date_trunc('minute', b.{col}), \
                jsonb_build_object('org', d.organization::text), \
                hashtextextended(d.organization::text, 0), \
                count(*)::bigint, 0, 0, 0, 0, NULL \
         FROM build b JOIN derivation d ON d.id = b.derivation \
         WHERE b.{col} IS NOT NULL \
           AND b.{col} >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ({filter}) \
         GROUP BY date_trunc('minute', b.{col}), d.organization \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET count = EXCLUDED.count",
        name = m.name,
        col = m.time_col,
        window = MINUTE_WINDOW,
        filter = m.filter,
    )
}

fn build_duration_sql(m: &BuildDuration) -> String {
    let ms = format!("extract(epoch from (b.{} - b.{})) * 1000", m.end_col, m.start_col);
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT gen_random_uuid(), '{name}', 0, date_trunc('minute', b.{end}), \
                jsonb_build_object('org', d.organization::text), \
                hashtextextended(d.organization::text, 0), \
                count(*)::bigint, sum({ms}), min({ms}), max({ms}), sum(power({ms}, 2)), NULL \
         FROM build b JOIN derivation d ON d.id = b.derivation \
         WHERE b.{end} IS NOT NULL AND b.{start} IS NOT NULL \
           AND b.{end} >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ({filter}) \
         GROUP BY date_trunc('minute', b.{end}), d.organization \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq",
        name = m.name,
        end = m.end_col,
        start = m.start_col,
        window = MINUTE_WINDOW,
        filter = m.filter,
        ms = ms,
    )
}

fn eval_count_sql(m: &EvalCount) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT gen_random_uuid(), '{name}', 0, date_trunc('minute', e.finished_at), \
                jsonb_build_object('org', p.organization::text), \
                hashtextextended(p.organization::text, 0), \
                count(*)::bigint, 0, 0, 0, 0, NULL \
         FROM evaluation e JOIN project p ON p.id = e.project \
         WHERE e.finished_at IS NOT NULL \
           AND e.finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ({filter}) \
         GROUP BY date_trunc('minute', e.finished_at), p.organization \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET count = EXCLUDED.count",
        name = m.name,
        window = MINUTE_WINDOW,
        filter = m.filter,
    )
}

fn cascade_sql(target: i16, source: i16, unit: &str, window: &str) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT gen_random_uuid(), metric, {target}, date_trunc('{unit}', bucket_start), \
                scope, scope_hash, \
                sum(count)::bigint, sum(sum), min(min), max(max), sum(sum_sq), NULL \
         FROM metric_rollup \
         WHERE granularity = {source} \
           AND bucket_start >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
         GROUP BY metric, scope_hash, scope, date_trunc('{unit}', bucket_start) \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq"
    )
}
