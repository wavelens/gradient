/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Windowed instance-wide metrics snapshot fed into resource-aware scoring.

use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use tracing::error;

/// In-memory scheduler counts the loop already holds; merged into the snapshot.
pub struct InstanceCounts {
    pub active_builds: u32,
    pub pending_builds: u32,
    pub total_workers: u32,
    pub idle_workers: u32,
}

/// Build a [`score::Windowed`] from a 5m/1h/24h column triple.
fn windowed(w5m: f64, w1h: f64, w24h: f64) -> score::Windowed {
    score::Windowed { w5m, w1h, w24h }
}

#[derive(Debug, Default, FromQueryResult)]
struct MetricRow {
    peak_ram_5m: f64,
    peak_ram_1h: f64,
    peak_ram_24h: f64,
    cpu_time_5m: f64,
    cpu_time_1h: f64,
    cpu_time_24h: f64,
    cpu_pct_5m: f64,
    cpu_pct_1h: f64,
    cpu_pct_24h: f64,
    disk_5m: f64,
    disk_1h: f64,
    disk_24h: f64,
    network_5m: f64,
    network_1h: f64,
    network_24h: f64,
    build_time_5m: f64,
    build_time_1h: f64,
    build_time_24h: f64,
    closure_5m: f64,
    closure_1h: f64,
    closure_24h: f64,
    oom_5m: f64,
    oom_1h: f64,
    oom_24h: f64,
    completed_5m: f64,
    completed_1h: f64,
    completed_24h: f64,
}

#[derive(Debug, Default, FromQueryResult)]
struct DispatchRow {
    wait_5m: f64,
    wait_1h: f64,
    wait_24h: f64,
    nar_5m: f64,
    nar_1h: f64,
    nar_24h: f64,
    miss_5m: f64,
    miss_1h: f64,
    miss_24h: f64,
    dep_5m: f64,
    dep_1h: f64,
    dep_24h: f64,
}

/// Compute a fresh windowed snapshot from `derivation_metric` + `dispatched_job`.
/// Each query degrades independently — errors are logged and that query's windows zero; counts always survive.
pub async fn compute_instance_context(
    db: &impl ConnectionTrait,
    counts: InstanceCounts,
    now: chrono::NaiveDateTime,
) -> score::InstanceContext {
    let c5m = now - chrono::Duration::minutes(5);
    let c1h = now - chrono::Duration::hours(1);
    let c24h = now - chrono::Duration::hours(24);

    let metric_sql = r#"
        SELECT
          COALESCE(AVG(peak_ram_mb)    FILTER (WHERE created_at >= $1), 0) AS peak_ram_5m,
          COALESCE(AVG(peak_ram_mb)    FILTER (WHERE created_at >= $2), 0) AS peak_ram_1h,
          COALESCE(AVG(peak_ram_mb)    FILTER (WHERE created_at >= $3), 0) AS peak_ram_24h,
          COALESCE(AVG(cpu_time_ms)    FILTER (WHERE created_at >= $1), 0) AS cpu_time_5m,
          COALESCE(AVG(cpu_time_ms)    FILTER (WHERE created_at >= $2), 0) AS cpu_time_1h,
          COALESCE(AVG(cpu_time_ms)    FILTER (WHERE created_at >= $3), 0) AS cpu_time_24h,
          COALESCE(AVG(avg_cpu_pct)    FILTER (WHERE created_at >= $1), 0) AS cpu_pct_5m,
          COALESCE(AVG(avg_cpu_pct)    FILTER (WHERE created_at >= $2), 0) AS cpu_pct_1h,
          COALESCE(AVG(avg_cpu_pct)    FILTER (WHERE created_at >= $3), 0) AS cpu_pct_24h,
          COALESCE(AVG(disk_read_bytes + disk_write_bytes) FILTER (WHERE created_at >= $1), 0) AS disk_5m,
          COALESCE(AVG(disk_read_bytes + disk_write_bytes) FILTER (WHERE created_at >= $2), 0) AS disk_1h,
          COALESCE(AVG(disk_read_bytes + disk_write_bytes) FILTER (WHERE created_at >= $3), 0) AS disk_24h,
          COALESCE(AVG(peak_network_mbps) FILTER (WHERE created_at >= $1), 0) AS network_5m,
          COALESCE(AVG(peak_network_mbps) FILTER (WHERE created_at >= $2), 0) AS network_1h,
          COALESCE(AVG(peak_network_mbps) FILTER (WHERE created_at >= $3), 0) AS network_24h,
          COALESCE(AVG(build_time_ms)  FILTER (WHERE created_at >= $1), 0) AS build_time_5m,
          COALESCE(AVG(build_time_ms)  FILTER (WHERE created_at >= $2), 0) AS build_time_1h,
          COALESCE(AVG(build_time_ms)  FILTER (WHERE created_at >= $3), 0) AS build_time_24h,
          COALESCE(AVG(closure_size)   FILTER (WHERE created_at >= $1), 0) AS closure_5m,
          COALESCE(AVG(closure_size)   FILTER (WHERE created_at >= $2), 0) AS closure_1h,
          COALESCE(AVG(closure_size)   FILTER (WHERE created_at >= $3), 0) AS closure_24h,
          COALESCE(AVG(CASE WHEN oom_killed THEN 1.0 ELSE 0.0 END) FILTER (WHERE created_at >= $1), 0) AS oom_5m,
          COALESCE(AVG(CASE WHEN oom_killed THEN 1.0 ELSE 0.0 END) FILTER (WHERE created_at >= $2), 0) AS oom_1h,
          COALESCE(AVG(CASE WHEN oom_killed THEN 1.0 ELSE 0.0 END) FILTER (WHERE created_at >= $3), 0) AS oom_24h,
          COALESCE(COUNT(*) FILTER (WHERE created_at >= $1), 0)::float8 AS completed_5m,
          COALESCE(COUNT(*) FILTER (WHERE created_at >= $2), 0)::float8 AS completed_1h,
          COALESCE(COUNT(*) FILTER (WHERE created_at >= $3), 0)::float8 AS completed_24h
        FROM derivation_metric
        WHERE created_at >= $3
    "#;

    let metric = match MetricRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        metric_sql,
        [c5m.into(), c1h.into(), c24h.into()],
    ))
    .one(db)
    .await
    {
        Ok(row) => row.unwrap_or_default(),
        Err(e) => {
            error!(error = %e, "instance metrics: derivation_metric query failed");
            MetricRow::default()
        }
    };

    let dispatch_sql = r#"
        SELECT
          COALESCE(AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $1), 0) AS wait_5m,
          COALESCE(AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $2), 0) AS wait_1h,
          COALESCE(AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $3), 0) AS wait_24h,
          COALESCE(AVG((job_context->>'missing_nar_size')::bigint / 1048576.0) FILTER (WHERE dispatched_at >= $1), 0) AS nar_5m,
          COALESCE(AVG((job_context->>'missing_nar_size')::bigint / 1048576.0) FILTER (WHERE dispatched_at >= $2), 0) AS nar_1h,
          COALESCE(AVG((job_context->>'missing_nar_size')::bigint / 1048576.0) FILTER (WHERE dispatched_at >= $3), 0) AS nar_24h,
          COALESCE(AVG((job_context->>'missing_count')::int) FILTER (WHERE dispatched_at >= $1), 0) AS miss_5m,
          COALESCE(AVG((job_context->>'missing_count')::int) FILTER (WHERE dispatched_at >= $2), 0) AS miss_1h,
          COALESCE(AVG((job_context->>'missing_count')::int) FILTER (WHERE dispatched_at >= $3), 0) AS miss_24h,
          COALESCE(AVG((job_context->>'dependency_count')::int) FILTER (WHERE dispatched_at >= $1), 0) AS dep_5m,
          COALESCE(AVG((job_context->>'dependency_count')::int) FILTER (WHERE dispatched_at >= $2), 0) AS dep_1h,
          COALESCE(AVG((job_context->>'dependency_count')::int) FILTER (WHERE dispatched_at >= $3), 0) AS dep_24h
        FROM dispatched_job
        WHERE kind = 1 AND ready_at IS NOT NULL AND dispatched_at >= $3
    "#;

    let dispatch = match DispatchRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        dispatch_sql,
        [c5m.into(), c1h.into(), c24h.into()],
    ))
    .one(db)
    .await
    {
        Ok(row) => row.unwrap_or_default(),
        Err(e) => {
            error!(error = %e, "instance metrics: dispatched_job query failed");
            DispatchRow::default()
        }
    };

    score::InstanceContext {
        wait_secs: windowed(dispatch.wait_5m, dispatch.wait_1h, dispatch.wait_24h),
        build_time_ms: windowed(metric.build_time_5m, metric.build_time_1h, metric.build_time_24h),
        peak_ram_mb: windowed(metric.peak_ram_5m, metric.peak_ram_1h, metric.peak_ram_24h),
        cpu_time_ms: windowed(metric.cpu_time_5m, metric.cpu_time_1h, metric.cpu_time_24h),
        avg_cpu_pct: windowed(metric.cpu_pct_5m, metric.cpu_pct_1h, metric.cpu_pct_24h),
        disk_bytes: windowed(metric.disk_5m, metric.disk_1h, metric.disk_24h),
        network_mbps: windowed(metric.network_5m, metric.network_1h, metric.network_24h),
        oom_rate: windowed(metric.oom_5m, metric.oom_1h, metric.oom_24h),
        closure_size: windowed(metric.closure_5m, metric.closure_1h, metric.closure_24h),
        nar_size_mb: windowed(dispatch.nar_5m, dispatch.nar_1h, dispatch.nar_24h),
        missing_paths: windowed(dispatch.miss_5m, dispatch.miss_1h, dispatch.miss_24h),
        dependency_cnt: windowed(dispatch.dep_5m, dispatch.dep_1h, dispatch.dep_24h),
        completed: windowed(metric.completed_5m, metric.completed_1h, metric.completed_24h),
        active_builds: counts.active_builds,
        pending_builds: counts.pending_builds,
        total_workers: counts.total_workers,
        idle_workers: counts.idle_workers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    /// `MockDatabase` replays raw column maps for the two statements, so the
    /// test pins the column→field mapping and count wiring. The SQL aggregation
    /// (FILTER windows, jsonb extraction) is validated in CI against Postgres.
    #[tokio::test]
    async fn maps_columns_and_counts_into_snapshot() {
        let f = |name: &str, v: f64| (name.to_owned(), Value::from(v));
        let metric: BTreeMap<String, Value> = [
            f("peak_ram_5m", 100.0), f("peak_ram_1h", 200.0), f("peak_ram_24h", 300.0),
            f("cpu_time_5m", 1.0), f("cpu_time_1h", 2.0), f("cpu_time_24h", 3.0),
            f("cpu_pct_5m", 10.0), f("cpu_pct_1h", 20.0), f("cpu_pct_24h", 30.0),
            f("disk_5m", 4.0), f("disk_1h", 5.0), f("disk_24h", 6.0),
            f("network_5m", 7.0), f("network_1h", 8.0), f("network_24h", 9.0),
            f("build_time_5m", 11.0), f("build_time_1h", 12.0), f("build_time_24h", 13.0),
            f("closure_5m", 14.0), f("closure_1h", 15.0), f("closure_24h", 16.0),
            f("oom_5m", 0.1), f("oom_1h", 0.2), f("oom_24h", 0.3),
            f("completed_5m", 4.0), f("completed_1h", 40.0), f("completed_24h", 400.0),
        ]
        .into_iter()
        .collect();
        let dispatch: BTreeMap<String, Value> = [
            f("wait_5m", 1.5), f("wait_1h", 2.5), f("wait_24h", 3.5),
            f("nar_5m", 17.0), f("nar_1h", 18.0), f("nar_24h", 19.0),
            f("miss_5m", 1.0), f("miss_1h", 2.0), f("miss_24h", 3.0),
            f("dep_5m", 21.0), f("dep_1h", 22.0), f("dep_24h", 23.0),
        ]
        .into_iter()
        .collect();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![metric]])
            .append_query_results([vec![dispatch]])
            .into_connection();

        let counts = InstanceCounts {
            active_builds: 2,
            pending_builds: 3,
            total_workers: 5,
            idle_workers: 1,
        };
        let ic = compute_instance_context(&db, counts, gradient_core::types::now()).await;

        assert_eq!(ic.peak_ram_mb, windowed(100.0, 200.0, 300.0));
        assert_eq!(ic.build_time_ms.w1h, 12.0);
        assert_eq!(ic.completed.w24h, 400.0);
        assert_eq!(ic.wait_secs, windowed(1.5, 2.5, 3.5));
        assert_eq!(ic.nar_size_mb.w24h, 19.0);
        assert_eq!(ic.dependency_cnt.w1h, 22.0);
        assert_eq!(ic.active_builds, 2);
        assert_eq!(ic.pending_builds, 3);
        assert_eq!(ic.total_workers, 5);
        assert_eq!(ic.idle_workers, 1);
    }
}
