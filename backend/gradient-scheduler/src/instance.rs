/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Windowed instance-wide metrics snapshot fed into resource-aware scoring.

use std::collections::HashMap;

use gradient_types::ids::ProjectId;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use tracing::error;

/// In-memory scheduler counts the loop already holds; merged into the snapshot.
pub struct InstanceCounts {
    pub active_builds: u32,
    pub pending_builds: u32,
    pub total_workers: u32,
    pub idle_workers: u32,
}

/// Build a [`gradient_score::Windowed`] from a 5m/1h/24h column triple.
/// `None` (SQL NULL: no samples in the window) stays `None` - a window with no
/// data must be distinguishable from a measured zero.
fn windowed(
    w5m: Option<f64>,
    w1h: Option<f64>,
    w24h: Option<f64>,
) -> gradient_score::Windowed {
    gradient_score::Windowed { w5m, w1h, w24h }
}

#[derive(Debug, Default, FromQueryResult)]
struct MetricRow {
    peak_ram_5m: Option<f64>,
    peak_ram_1h: Option<f64>,
    peak_ram_24h: Option<f64>,
    cpu_time_5m: Option<f64>,
    cpu_time_1h: Option<f64>,
    cpu_time_24h: Option<f64>,
    cpu_pct_5m: Option<f64>,
    cpu_pct_1h: Option<f64>,
    cpu_pct_24h: Option<f64>,
    disk_5m: Option<f64>,
    disk_1h: Option<f64>,
    disk_24h: Option<f64>,
    network_5m: Option<f64>,
    network_1h: Option<f64>,
    network_24h: Option<f64>,
    build_time_5m: Option<f64>,
    build_time_1h: Option<f64>,
    build_time_24h: Option<f64>,
    closure_5m: Option<f64>,
    closure_1h: Option<f64>,
    closure_24h: Option<f64>,
    oom_5m: Option<f64>,
    oom_1h: Option<f64>,
    oom_24h: Option<f64>,
    completed_5m: f64,
    completed_1h: f64,
    completed_24h: f64,
}

#[derive(Debug, Default, FromQueryResult)]
struct DispatchRow {
    wait_5m: Option<f64>,
    wait_1h: Option<f64>,
    wait_24h: Option<f64>,
    nar_5m: Option<f64>,
    nar_1h: Option<f64>,
    nar_24h: Option<f64>,
    miss_5m: Option<f64>,
    miss_1h: Option<f64>,
    miss_24h: Option<f64>,
    dep_5m: Option<f64>,
    dep_1h: Option<f64>,
    dep_24h: Option<f64>,
}

/// Compute a fresh windowed snapshot from `derivation_metric` + `dispatched_job`.
/// Each query degrades independently - errors are logged and that query's windows read absent; counts always survive.
pub async fn compute_instance_context(
    db: &impl ConnectionTrait,
    counts: InstanceCounts,
    now: chrono::NaiveDateTime,
) -> gradient_score::InstanceContext {
    let c5m = now - chrono::Duration::minutes(5);
    let c1h = now - chrono::Duration::hours(1);
    let c24h = now - chrono::Duration::hours(24);

    let metric_sql = r#"
        SELECT
          (AVG(peak_ram_mb)    FILTER (WHERE created_at >= $1))::float8 AS peak_ram_5m,
          (AVG(peak_ram_mb)    FILTER (WHERE created_at >= $2))::float8 AS peak_ram_1h,
          (AVG(peak_ram_mb)    FILTER (WHERE created_at >= $3))::float8 AS peak_ram_24h,
          (AVG(cpu_time_ms)    FILTER (WHERE created_at >= $1))::float8 AS cpu_time_5m,
          (AVG(cpu_time_ms)    FILTER (WHERE created_at >= $2))::float8 AS cpu_time_1h,
          (AVG(cpu_time_ms)    FILTER (WHERE created_at >= $3))::float8 AS cpu_time_24h,
          (AVG(avg_cpu_pct)    FILTER (WHERE created_at >= $1))::float8 AS cpu_pct_5m,
          (AVG(avg_cpu_pct)    FILTER (WHERE created_at >= $2))::float8 AS cpu_pct_1h,
          (AVG(avg_cpu_pct)    FILTER (WHERE created_at >= $3))::float8 AS cpu_pct_24h,
          (AVG(disk_read_bytes + disk_write_bytes) FILTER (WHERE created_at >= $1))::float8 AS disk_5m,
          (AVG(disk_read_bytes + disk_write_bytes) FILTER (WHERE created_at >= $2))::float8 AS disk_1h,
          (AVG(disk_read_bytes + disk_write_bytes) FILTER (WHERE created_at >= $3))::float8 AS disk_24h,
          (AVG(peak_network_mbps) FILTER (WHERE created_at >= $1))::float8 AS network_5m,
          (AVG(peak_network_mbps) FILTER (WHERE created_at >= $2))::float8 AS network_1h,
          (AVG(peak_network_mbps) FILTER (WHERE created_at >= $3))::float8 AS network_24h,
          (AVG(build_time_ms)  FILTER (WHERE created_at >= $1))::float8 AS build_time_5m,
          (AVG(build_time_ms)  FILTER (WHERE created_at >= $2))::float8 AS build_time_1h,
          (AVG(build_time_ms)  FILTER (WHERE created_at >= $3))::float8 AS build_time_24h,
          (AVG(closure_size)   FILTER (WHERE created_at >= $1))::float8 AS closure_5m,
          (AVG(closure_size)   FILTER (WHERE created_at >= $2))::float8 AS closure_1h,
          (AVG(closure_size)   FILTER (WHERE created_at >= $3))::float8 AS closure_24h,
          (AVG(CASE WHEN oom_killed THEN 1.0 ELSE 0.0 END) FILTER (WHERE created_at >= $1))::float8 AS oom_5m,
          (AVG(CASE WHEN oom_killed THEN 1.0 ELSE 0.0 END) FILTER (WHERE created_at >= $2))::float8 AS oom_1h,
          (AVG(CASE WHEN oom_killed THEN 1.0 ELSE 0.0 END) FILTER (WHERE created_at >= $3))::float8 AS oom_24h,
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
          (AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $1))::float8 AS wait_5m,
          (AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $2))::float8 AS wait_1h,
          (AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $3))::float8 AS wait_24h,
          (AVG((job_context->>'missing_nar_size')::bigint / 1048576.0) FILTER (WHERE dispatched_at >= $1))::float8 AS nar_5m,
          (AVG((job_context->>'missing_nar_size')::bigint / 1048576.0) FILTER (WHERE dispatched_at >= $2))::float8 AS nar_1h,
          (AVG((job_context->>'missing_nar_size')::bigint / 1048576.0) FILTER (WHERE dispatched_at >= $3))::float8 AS nar_24h,
          (AVG((job_context->>'missing_count')::int) FILTER (WHERE dispatched_at >= $1))::float8 AS miss_5m,
          (AVG((job_context->>'missing_count')::int) FILTER (WHERE dispatched_at >= $2))::float8 AS miss_1h,
          (AVG((job_context->>'missing_count')::int) FILTER (WHERE dispatched_at >= $3))::float8 AS miss_24h,
          (AVG((job_context->>'dependency_count')::int) FILTER (WHERE dispatched_at >= $1))::float8 AS dep_5m,
          (AVG((job_context->>'dependency_count')::int) FILTER (WHERE dispatched_at >= $2))::float8 AS dep_1h,
          (AVG((job_context->>'dependency_count')::int) FILTER (WHERE dispatched_at >= $3))::float8 AS dep_24h
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

    gradient_score::InstanceContext {
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
        completed: windowed(Some(metric.completed_5m), Some(metric.completed_1h), Some(metric.completed_24h)),
        active_builds: counts.active_builds,
        pending_builds: counts.pending_builds,
        total_workers: counts.total_workers,
        idle_workers: counts.idle_workers,
    }
}

#[derive(Debug, Default, FromQueryResult)]
struct EvalHistoryRow {
    project: ProjectId,
    p95_ram: f64,
    samples: i64,
}

/// Per-project p95 of evaluation peak RSS over the last 24h, fed into
/// `ResourceFitRule` so heavy evals route to big-RAM workers.
pub async fn compute_eval_history(
    db: &impl ConnectionTrait,
    now: chrono::NaiveDateTime,
) -> HashMap<ProjectId, gradient_score::HistoryPrediction> {
    let since = now - chrono::Duration::hours(24);
    let sql = r#"
        SELECT e.project AS project,
               COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY m.peak_rss_mb), 0)::float8 AS p95_ram,
               COUNT(*)::bigint AS samples
        FROM evaluation_metric m
        JOIN evaluation e ON e.id = m.evaluation
        WHERE m.created_at >= $1 AND e.project IS NOT NULL
        GROUP BY e.project
    "#;

    let rows = match EvalHistoryRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        [since.into()],
    ))
    .all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "eval history query failed");
            return HashMap::new();
        }
    };

    rows.into_iter()
        .map(|r| {
            (r.project, gradient_score::HistoryPrediction {
                predicted_peak_ram_mb: r.p95_ram.max(0.0) as u64,
                samples: r.samples.max(0) as u32,
                ..Default::default()
            })
        })
        .collect()
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
        let ic = compute_instance_context(&db, counts, gradient_types::now()).await;

        assert_eq!(ic.peak_ram_mb, windowed(Some(100.0), Some(200.0), Some(300.0)));
        assert_eq!(ic.build_time_ms.w1h, Some(12.0));
        assert_eq!(ic.completed.w24h, Some(400.0));
        assert_eq!(ic.wait_secs, windowed(Some(1.5), Some(2.5), Some(3.5)));
        assert_eq!(ic.nar_size_mb.w24h, Some(19.0));
        assert_eq!(ic.dependency_cnt.w1h, Some(22.0));
        assert_eq!(ic.active_builds, 2);
        assert_eq!(ic.pending_builds, 3);
        assert_eq!(ic.total_workers, 5);
        assert_eq!(ic.idle_workers, 1);
    }

    /// `MockDatabase` replays one grouped row; pins the project->prediction
    /// mapping. The percentile aggregation is validated in CI against Postgres.
    #[tokio::test]
    async fn eval_history_maps_row_into_prediction() {
        let pid = ProjectId::now_v7();
        let row: BTreeMap<String, Value> = [
            ("project".to_owned(), Value::from(pid.into_inner())),
            ("p95_ram".to_owned(), Value::from(42_000.0_f64)),
            ("samples".to_owned(), Value::from(7_i64)),
        ]
        .into_iter()
        .collect();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();

        let history = compute_eval_history(&db, gradient_types::now()).await;
        let h = history.get(&pid).expect("project present");
        assert_eq!(h.predicted_peak_ram_mb, 42_000);
        assert_eq!(h.samples, 7);
    }
}
