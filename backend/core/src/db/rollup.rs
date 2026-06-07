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

/// (target_granularity, source_granularity, date_trunc unit, trailing window).
const CASCADES: &[(i16, i16, &str, &str)] = &[
    (1, 0, "hour", "3 hours"),
    (2, 1, "day", "2 days"),
    (3, 2, "week", "2 weeks"),
];

const MINUTE_WINDOW: &str = "15 minutes";

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
