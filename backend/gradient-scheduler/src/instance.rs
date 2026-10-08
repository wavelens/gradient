/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use gradient_types::ids::TaskId;
use gradient_wire::types::FlakeStep;
use sea_orm::{ConnectionTrait, FromQueryResult};
use tracing::error;

pub struct InstanceCounts {
    pub active_builds: u32,
    pub pending_builds: u32,
    pub total_workers: u32,
    pub idle_workers: u32,
    pub cpu_core_score_mean: Option<f64>,
    pub upload_speed_mean_mbps: Option<f64>,
    pub download_speed_mean_mbps: Option<f64>,
    pub download_slots: u32,
    pub upload_slots: u32,
}

/// A window with no data must stay distinguishable from a measured zero.
fn windowed(
    w5m: Option<f64>,
    w1h: Option<f64>,
    w24h: Option<f64>,
) -> gradient_pool::score::Windowed {
    gradient_pool::score::Windowed { w5m, w1h, w24h }
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
    build_time_5m: Option<f64>,
    build_time_1h: Option<f64>,
    build_time_24h: Option<f64>,
    build_median_5m: Option<f64>,
    build_median_1h: Option<f64>,
    build_median_24h: Option<f64>,
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
struct AssignmentWindowRow {
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

gradient_db::sql! {
    INSTANCE_METRIC_WINDOWS = r#"
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
          (AVG(build_time_ms)  FILTER (WHERE created_at >= $1))::float8 AS build_time_5m,
          (AVG(build_time_ms)  FILTER (WHERE created_at >= $2))::float8 AS build_time_1h,
          (AVG(build_time_ms)  FILTER (WHERE created_at >= $3))::float8 AS build_time_24h,
          (percentile_cont(0.5) WITHIN GROUP (ORDER BY build_time_ms) FILTER (WHERE created_at >= $1))::float8 AS build_median_5m,
          (percentile_cont(0.5) WITHIN GROUP (ORDER BY build_time_ms) FILTER (WHERE created_at >= $2))::float8 AS build_median_1h,
          (percentile_cont(0.5) WITHIN GROUP (ORDER BY build_time_ms) FILTER (WHERE created_at >= $3))::float8 AS build_median_24h,
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
    "#,
        params = [Now, Now, Now];
}

gradient_db::sql_fn! {
    /// The gate must plan against the same generated fragment the call site is executing.
    INSTANCE_DISPATCH_WINDOWS = assignment_windows_sql,
        params = [Now, Now, Now];
}

fn assignment_windows_sql() -> String {
    format!(
        r#"
        SELECT
          (AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $1))::float8 AS wait_5m,
          (AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $2))::float8 AS wait_1h,
          (AVG(EXTRACT(EPOCH FROM (dispatched_at - ready_at))) FILTER (WHERE dispatched_at >= $3))::float8 AS wait_24h,
          (AVG(missing_nar_size / 1048576.0) FILTER (WHERE dispatched_at >= $1))::float8 AS nar_5m,
          (AVG(missing_nar_size / 1048576.0) FILTER (WHERE dispatched_at >= $2))::float8 AS nar_1h,
          (AVG(missing_nar_size / 1048576.0) FILTER (WHERE dispatched_at >= $3))::float8 AS nar_24h,
          (AVG(missing_count) FILTER (WHERE dispatched_at >= $1))::float8 AS miss_5m,
          (AVG(missing_count) FILTER (WHERE dispatched_at >= $2))::float8 AS miss_1h,
          (AVG(missing_count) FILTER (WHERE dispatched_at >= $3))::float8 AS miss_24h,
          (AVG(dependency_count) FILTER (WHERE dispatched_at >= $1))::float8 AS dep_5m,
          (AVG(dependency_count) FILTER (WHERE dispatched_at >= $2))::float8 AS dep_1h,
          (AVG(dependency_count) FILTER (WHERE dispatched_at >= $3))::float8 AS dep_24h
        FROM dispatched_job
        WHERE kind = {kind} AND ready_at IS NOT NULL AND dispatched_at >= $3
    "#,
        kind = i16::from(gradient_entity::dispatched_job::DispatchedJobKind::Build)
    )
}

#[derive(Debug, Default, FromQueryResult)]
struct StorageRow {
    read_mbps: Option<f64>,
    write_mbps: Option<f64>,
}

gradient_db::sql! {
    STORAGE_PEAK_THROUGHPUT = r#"
        WITH span AS (
          SELECT p.phase,
                 d.finished_at - make_interval(secs => (d.worker_elapsed_ms - p.start_ms) / 1000.0) AS started,
                 d.finished_at - make_interval(secs => (d.worker_elapsed_ms - p.end_ms) / 1000.0) AS ended,
                 p.bytes * 8.0 / (1000.0 * (p.end_ms - p.start_ms)) AS mbps
          FROM dispatched_job_phase p
          JOIN dispatched_job d ON d.id = p.dispatched_job
          WHERE p.phase IN ($2, $3) AND p.created_at >= $1
            AND p.bytes >= 1048576 AND p.end_ms - p.start_ms >= 1000
            AND d.finished_at IS NOT NULL AND d.worker_elapsed_ms IS NOT NULL
        ),
        change AS (
          SELECT phase, started AS at, mbps AS delta FROM span
          UNION ALL
          SELECT phase, ended, -mbps FROM span
        ),
        running AS (
          SELECT phase, SUM(delta) OVER (PARTITION BY phase ORDER BY at, delta ROWS UNBOUNDED PRECEDING) AS mbps
          FROM change
        )
        SELECT (MAX(mbps) FILTER (WHERE phase = $2))::float8 AS read_mbps,
               (MAX(mbps) FILTER (WHERE phase = $3))::float8 AS write_mbps
        FROM running
    "#,
        params = [Now, Int(16), Int(12)];
}

#[derive(Debug, Default, FromQueryResult)]
struct CompressionRow {
    ratio: Option<f64>,
}

gradient_db::sql! {
    STORED_TO_NAR_RATIO = r#"
        SELECT (SUM(push.bytes)::float8 / NULLIF(SUM(c.bytes), 0))::float8 AS ratio
        FROM (
          SELECT dispatched_job, parent_seq, SUM(bytes) AS bytes
          FROM dispatched_job_phase
          WHERE phase = $2 AND created_at >= $1 AND parent_seq IS NOT NULL
          GROUP BY dispatched_job, parent_seq
        ) push
        JOIN dispatched_job_phase c
          ON c.dispatched_job = push.dispatched_job AND c.seq = push.parent_seq
        WHERE c.phase = $3 AND c.bytes > 0
    "#,
        params = [Now, Int(12), Int(11)];
}

const STORAGE_WINDOW_HOURS: i64 = 1;

#[derive(Debug, Default, FromQueryResult)]
struct PathFitRow {
    n: f64,
    sb: Option<f64>,
    sp: Option<f64>,
    sy: Option<f64>,
    sbb: Option<f64>,
    spp: Option<f64>,
    sbp: Option<f64>,
    sby: Option<f64>,
    spy: Option<f64>,
}

gradient_db::sql! {
    PREFETCH_TIME_FIT = r#"
        SELECT COUNT(*)::float8 AS n,
               SUM(b)::float8 AS sb, SUM(p)::float8 AS sp, SUM(y)::float8 AS sy,
               SUM(b * b)::float8 AS sbb, SUM(p * p)::float8 AS spp, SUM(b * p)::float8 AS sbp,
               SUM(b * y)::float8 AS sby, SUM(p * y)::float8 AS spy
        FROM (
          SELECT (end_ms - start_ms) / 1000.0 AS y, bytes / 1000000.0 AS b, paths::numeric AS p
          FROM dispatched_job_phase
          WHERE phase = $2 AND created_at >= $1
        ) span
    "#,
        params = [Now, Int(8)];
}

const PER_PATH_WINDOW_HOURS: i64 = 24;
const PER_PATH_MIN_SPANS: f64 = 100.0;

#[derive(Debug, Default, FromQueryResult)]
struct SubstituteFitRow {
    n: f64,
    sb: Option<f64>,
    sy: Option<f64>,
    sbb: Option<f64>,
    sby: Option<f64>,
}

gradient_db::sql! {
    SUBSTITUTE_TIME_FIT = r#"
        SELECT COUNT(*)::float8 AS n,
               SUM(b)::float8 AS sb, SUM(y)::float8 AS sy,
               SUM(b * b)::float8 AS sbb, SUM(b * y)::float8 AS sby
        FROM (
          SELECT (end_ms - start_ms) / 1000.0 AS y, bytes / 1000000.0 AS b
          FROM dispatched_job_phase
          WHERE phase = $2 AND created_at >= $1 AND bytes > 0
        ) span
    "#,
        params = [Now, Int(14)];
}

const SUBSTITUTE_MIN_SPANS: f64 = 20.0;

fn substitute_cost(row: &SubstituteFitRow) -> Option<gradient_pool::score::SubstituteCost> {
    if row.n < SUBSTITUTE_MIN_SPANS {
        return None;
    }

    let (sb, sy, sbb, sby) = (row.sb?, row.sy?, row.sbb?, row.sby?);
    let det = row.n * sbb - sb * sb;
    if det.abs() < f64::EPSILON {
        return None;
    }

    let secs_per_mb = (row.n * sby - sb * sy) / det;
    let per_path_secs = (sy - secs_per_mb * sb) / row.n;
    (secs_per_mb.is_finite() && secs_per_mb > 0.0).then_some(gradient_pool::score::SubstituteCost {
        per_path_secs: per_path_secs.max(0.0),
        secs_per_mb,
    })
}

fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn per_path_secs(row: &PathFitRow) -> Option<f64> {
    if row.n < PER_PATH_MIN_SPANS {
        return None;
    }

    let normal = [
        [row.n, row.sb?, row.sp?],
        [row.sb?, row.sbb?, row.sbp?],
        [row.sp?, row.sbp?, row.spp?],
    ];
    let det = det3(normal);
    if det.abs() < f64::EPSILON {
        return None;
    }

    let mut paths_column = normal;
    for (row_index, value) in [row.sy?, row.sby?, row.spy?].into_iter().enumerate() {
        paths_column[row_index][2] = value;
    }

    Some(det3(paths_column) / det).filter(|secs| secs.is_finite() && *secs > 0.0)
}

async fn learned_per_path_secs(
    db: &impl ConnectionTrait,
    now: chrono::NaiveDateTime,
) -> Option<f64> {
    use gradient_wire::types::JobPhase;

    let since = now - chrono::Duration::hours(PER_PATH_WINDOW_HOURS);
    let row = PathFitRow::find_by_statement(
        PREFETCH_TIME_FIT.bind([since.into(), JobPhase::Prefetch.as_i16().into()]),
    )
    .one(db)
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "instance metrics: per-path fit query failed");
        None
    })?;

    per_path_secs(&row)
}

async fn learned_substitute_cost(
    db: &impl ConnectionTrait,
    now: chrono::NaiveDateTime,
) -> Option<gradient_pool::score::SubstituteCost> {
    use gradient_wire::types::JobPhase;

    let since = now - chrono::Duration::hours(PER_PATH_WINDOW_HOURS);
    let row = SubstituteFitRow::find_by_statement(
        SUBSTITUTE_TIME_FIT.bind([since.into(), JobPhase::SubstituteFetch.as_i16().into()]),
    )
    .one(db)
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "instance metrics: substitute fit query failed");
        None
    })?;

    substitute_cost(&row)
}

async fn storage_throughput(
    db: &impl ConnectionTrait,
    now: chrono::NaiveDateTime,
) -> (StorageRow, Option<f64>) {
    use gradient_wire::types::JobPhase;

    let since = now - chrono::Duration::hours(STORAGE_WINDOW_HOURS);
    let peak = StorageRow::find_by_statement(STORAGE_PEAK_THROUGHPUT.bind([
        since.into(),
        JobPhase::NarFetch.as_i16().into(),
        JobPhase::NarPush.as_i16().into(),
    ]))
    .one(db)
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "instance metrics: storage throughput query failed");
        None
    })
    .unwrap_or_default();

    let ratio = CompressionRow::find_by_statement(STORED_TO_NAR_RATIO.bind([
        since.into(),
        JobPhase::NarPush.as_i16().into(),
        JobPhase::Compress.as_i16().into(),
    ]))
    .one(db)
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "instance metrics: compression ratio query failed");
        None
    })
    .and_then(|row| row.ratio);

    (peak, ratio)
}

pub async fn compute_instance_context(
    db: &impl ConnectionTrait,
    counts: InstanceCounts,
    now: chrono::NaiveDateTime,
) -> gradient_pool::score::InstanceContext {
    let c5m = now - chrono::Duration::minutes(5);
    let c1h = now - chrono::Duration::hours(1);
    let c24h = now - chrono::Duration::hours(24);

    let metric = match MetricRow::find_by_statement(INSTANCE_METRIC_WINDOWS.bind([
        c5m.into(),
        c1h.into(),
        c24h.into(),
    ]))
    .one(db)
    .await
    {
        Ok(row) => row.unwrap_or_default(),
        Err(e) => {
            error!(error = %e, "instance metrics: derivation_metric query failed");
            MetricRow::default()
        }
    };

    let assignment_id =
        match AssignmentWindowRow::find_by_statement(INSTANCE_DISPATCH_WINDOWS.bind([
            c5m.into(),
            c1h.into(),
            c24h.into(),
        ]))
        .one(db)
        .await
        {
            Ok(row) => row.unwrap_or_default(),
            Err(e) => {
                error!(error = %e, "instance metrics: dispatched_job query failed");
                AssignmentWindowRow::default()
            }
        };

    let (storage, compression_ratio) = storage_throughput(db, now).await;
    let per_path_secs = learned_per_path_secs(db, now).await;
    let substitute_cost = learned_substitute_cost(db, now).await;

    gradient_pool::score::InstanceContext {
        wait_secs: windowed(
            assignment_id.wait_5m,
            assignment_id.wait_1h,
            assignment_id.wait_24h,
        ),
        build_time_ms: windowed(
            metric.build_time_5m,
            metric.build_time_1h,
            metric.build_time_24h,
        ),
        build_time_median_ms: windowed(
            metric.build_median_5m,
            metric.build_median_1h,
            metric.build_median_24h,
        ),
        peak_ram_mb: windowed(metric.peak_ram_5m, metric.peak_ram_1h, metric.peak_ram_24h),
        cpu_time_ms: windowed(metric.cpu_time_5m, metric.cpu_time_1h, metric.cpu_time_24h),
        avg_cpu_pct: windowed(metric.cpu_pct_5m, metric.cpu_pct_1h, metric.cpu_pct_24h),
        disk_bytes: windowed(metric.disk_5m, metric.disk_1h, metric.disk_24h),
        oom_rate: windowed(metric.oom_5m, metric.oom_1h, metric.oom_24h),
        closure_size: windowed(metric.closure_5m, metric.closure_1h, metric.closure_24h),
        nar_size_mb: windowed(
            assignment_id.nar_5m,
            assignment_id.nar_1h,
            assignment_id.nar_24h,
        ),
        missing_paths: windowed(
            assignment_id.miss_5m,
            assignment_id.miss_1h,
            assignment_id.miss_24h,
        ),
        dependency_cnt: windowed(
            assignment_id.dep_5m,
            assignment_id.dep_1h,
            assignment_id.dep_24h,
        ),
        completed: windowed(
            Some(metric.completed_5m),
            Some(metric.completed_1h),
            Some(metric.completed_24h),
        ),
        active_builds: counts.active_builds,
        pending_builds: counts.pending_builds,
        total_workers: counts.total_workers,
        idle_workers: counts.idle_workers,
        cpu_core_score_mean: counts.cpu_core_score_mean,
        upload_speed_mean_mbps: counts.upload_speed_mean_mbps,
        download_speed_mean_mbps: counts.download_speed_mean_mbps,
        storage_read_mbps: storage.read_mbps,
        storage_write_mbps: storage.write_mbps,
        compression_ratio,
        per_path_secs,
        substitute_cost,
        download_slots: counts.download_slots,
        upload_slots: counts.upload_slots,
        ..Default::default()
    }
}

#[derive(Debug, Default, FromQueryResult)]
struct EvalHistoryRow {
    task: TaskId,
    p95_ram: f64,
    samples: i64,
}

gradient_db::sql! {
    EVAL_HISTORY_P95_RAM = r#"
        SELECT e.task AS task,
               COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY m.peak_rss_mb), 0)::float8 AS p95_ram,
               COUNT(*)::bigint AS samples
        FROM evaluation_metric m
        JOIN evaluation e ON e.id = m.evaluation
        WHERE m.created_at >= $1 AND e.task IS NOT NULL
        GROUP BY e.task
    "#,
        params = [Now];
}

#[derive(Debug, Default, FromQueryResult)]
struct EvalDurationRow {
    task: TaskId,
    fetch_ms: Option<f64>,
    fetch_samples: i64,
    evaluate_ms: Option<f64>,
    evaluate_samples: i64,
}

gradient_db::sql! {
    EVAL_HISTORY_DURATION = r#"
        WITH job AS (
          SELECT d.task, d.worker_elapsed_ms AS elapsed,
                 SUM(p.end_ms - p.start_ms) FILTER (WHERE p.phase = $4 AND p.parent_seq IS NULL) AS fetch_ms,
                 BOOL_OR(p.phase IN ($5, $6)) AS evaluated
          FROM dispatched_job d
          JOIN dispatched_job_phase p ON p.dispatched_job = d.id
          WHERE d.kind = $2 AND d.outcome = $3 AND d.dispatched_at >= $1
            AND d.task IS NOT NULL AND d.worker_elapsed_ms IS NOT NULL
          GROUP BY d.id, d.task, d.worker_elapsed_ms
        )
        SELECT task,
               AVG(fetch_ms)::float8 AS fetch_ms,
               COUNT(fetch_ms)::bigint AS fetch_samples,
               (AVG(elapsed - COALESCE(fetch_ms, 0)) FILTER (WHERE evaluated))::float8 AS evaluate_ms,
               (COUNT(*) FILTER (WHERE evaluated))::bigint AS evaluate_samples
        FROM job
        GROUP BY task
    "#,
        params = [Now, Int(0), Int(0), Int(0), Int(2), Int(3)];
}

const EVAL_DURATION_WINDOW_DAYS: i64 = 7;

#[derive(Debug, Default, Clone, Copy)]
struct EvalDurations {
    fetch_ms: Option<u64>,
    evaluate_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub struct EvalHistory {
    tasks: HashMap<TaskId, gradient_pool::score::HistoryPrediction>,
    durations: HashMap<TaskId, EvalDurations>,
    fleet: EvalDurations,
}

impl EvalHistory {
    pub fn for_job(
        &self,
        task: TaskId,
        steps: &[FlakeStep],
    ) -> gradient_pool::score::HistoryPrediction {
        let mut history = self.tasks.get(&task).copied().unwrap_or_default();
        let own = self.durations.get(&task).copied().unwrap_or_default();
        let mut from_fleet_mean = false;
        let mut part = |runs: bool, own: Option<u64>, fleet: Option<u64>| {
            if !runs {
                return Some(0);
            }
            own.or_else(|| {
                from_fleet_mean = true;
                fleet
            })
        };
        let fetch_ms = part(
            steps.contains(&FlakeStep::FetchFlake),
            own.fetch_ms,
            self.fleet.fetch_ms,
        );
        let evaluate_ms = part(
            steps.iter().any(|s| *s != FlakeStep::FetchFlake),
            own.evaluate_ms,
            self.fleet.evaluate_ms,
        );

        let expected_ms = fetch_ms.zip(evaluate_ms).map(|(f, e)| f + e);
        history.uncontended_build_time_ms = expected_ms;
        history.build_time_ms = expected_ms.filter(|_| !from_fleet_mean);
        history.from_fleet_mean = from_fleet_mean && expected_ms.is_some();
        history
    }

    fn from_rows(ram: Vec<EvalHistoryRow>, rows: Vec<EvalDurationRow>) -> Self {
        let tasks = ram
            .into_iter()
            .map(|r| {
                (
                    r.task,
                    gradient_pool::score::HistoryPrediction {
                        predicted_peak_ram_mb: Some(r.p95_ram.max(0.0) as u64),
                        samples: r.samples.max(0) as u32,
                        ..Default::default()
                    },
                )
            })
            .collect();

        let mean = |pick: fn(&EvalDurationRow) -> (Option<f64>, i64)| {
            let (sum, runs) = rows
                .iter()
                .map(pick)
                .fold((0.0, 0.0), |(sum, runs), (ms, n)| {
                    (sum + ms.unwrap_or(0.0).max(0.0) * n as f64, runs + n as f64)
                });
            (runs > 0.0).then(|| (sum / runs) as u64)
        };
        let fleet = EvalDurations {
            fetch_ms: mean(|r| (r.fetch_ms, r.fetch_samples)),
            evaluate_ms: mean(|r| (r.evaluate_ms, r.evaluate_samples)),
        };
        let durations = rows
            .iter()
            .map(|r| {
                let ms = |v: Option<f64>| v.map(|ms| ms.max(0.0) as u64);
                (
                    r.task,
                    EvalDurations {
                        fetch_ms: ms(r.fetch_ms),
                        evaluate_ms: ms(r.evaluate_ms),
                    },
                )
            })
            .collect();

        Self {
            tasks,
            durations,
            fleet,
        }
    }
}

pub async fn compute_eval_history(
    db: &impl ConnectionTrait,
    now: chrono::NaiveDateTime,
) -> EvalHistory {
    use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
    use gradient_wire::types::JobPhase;

    let since = now - chrono::Duration::hours(24);
    let ram = EvalHistoryRow::find_by_statement(EVAL_HISTORY_P95_RAM.bind([since.into()]))
        .all(db)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "eval history query failed");
            Vec::new()
        });

    let durations = EvalDurationRow::find_by_statement(EVAL_HISTORY_DURATION.bind([
        (now - chrono::Duration::days(EVAL_DURATION_WINDOW_DAYS)).into(),
        i16::from(DispatchedJobKind::Eval).into(),
        i16::from(DispatchedJobOutcome::Completed).into(),
        JobPhase::Fetch.as_i16().into(),
        JobPhase::EvalFlake.as_i16().into(),
        JobPhase::EvalDerivations.as_i16().into(),
    ]))
    .all(db)
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "eval duration query failed");
        Vec::new()
    });

    EvalHistory::from_rows(ram, durations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn maps_columns_and_counts_into_snapshot() {
        let f = |name: &str, v: f64| (name.to_owned(), Value::from(v));
        let metric: BTreeMap<String, Value> = [
            f("peak_ram_5m", 100.0),
            f("peak_ram_1h", 200.0),
            f("peak_ram_24h", 300.0),
            f("cpu_time_5m", 1.0),
            f("cpu_time_1h", 2.0),
            f("cpu_time_24h", 3.0),
            f("cpu_pct_5m", 10.0),
            f("cpu_pct_1h", 20.0),
            f("cpu_pct_24h", 30.0),
            f("disk_5m", 4.0),
            f("disk_1h", 5.0),
            f("disk_24h", 6.0),
            f("build_time_5m", 11.0),
            f("build_time_1h", 12.0),
            f("build_time_24h", 13.0),
            f("build_median_5m", 0.1),
            f("build_median_1h", 0.2),
            f("build_median_24h", 0.3),
            f("closure_5m", 14.0),
            f("closure_1h", 15.0),
            f("closure_24h", 16.0),
            f("oom_5m", 0.1),
            f("oom_1h", 0.2),
            f("oom_24h", 0.3),
            f("completed_5m", 4.0),
            f("completed_1h", 40.0),
            f("completed_24h", 400.0),
        ]
        .into_iter()
        .collect();
        let assignment_id: BTreeMap<String, Value> = [
            f("wait_5m", 1.5),
            f("wait_1h", 2.5),
            f("wait_24h", 3.5),
            f("nar_5m", 17.0),
            f("nar_1h", 18.0),
            f("nar_24h", 19.0),
            f("miss_5m", 1.0),
            f("miss_1h", 2.0),
            f("miss_24h", 3.0),
            f("dep_5m", 21.0),
            f("dep_1h", 22.0),
            f("dep_24h", 23.0),
        ]
        .into_iter()
        .collect();

        let storage: BTreeMap<String, Value> = [f("read_mbps", 1_200.0), f("write_mbps", 900.0)]
            .into_iter()
            .collect();
        let compression: BTreeMap<String, Value> = [f("ratio", 0.4)].into_iter().collect();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![metric]])
            .append_query_results([vec![assignment_id]])
            .append_query_results([vec![storage]])
            .append_query_results([vec![compression]])
            .append_query_results([vec![BTreeMap::from([f("n", 0.0)])]])
            .append_query_results([vec![BTreeMap::from([f("n", 0.0)])]])
            .into_connection();

        let counts = InstanceCounts {
            active_builds: 2,
            pending_builds: 3,
            total_workers: 5,
            idle_workers: 1,
            cpu_core_score_mean: None,
            upload_speed_mean_mbps: Some(400.0),
            download_speed_mean_mbps: None,
            download_slots: 16,
            upload_slots: 8,
        };
        let ic = compute_instance_context(&db, counts, gradient_types::now()).await;

        assert_eq!(
            ic.peak_ram_mb,
            windowed(Some(100.0), Some(200.0), Some(300.0))
        );
        assert_eq!(ic.build_time_ms.w1h, Some(12.0));
        assert_eq!(ic.build_time_median_ms.w1h, Some(0.2));
        assert_eq!(ic.completed.w24h, Some(400.0));
        assert_eq!(ic.wait_secs, windowed(Some(1.5), Some(2.5), Some(3.5)));
        assert_eq!(ic.nar_size_mb.w24h, Some(19.0));
        assert_eq!(ic.dependency_cnt.w1h, Some(22.0));
        assert_eq!(ic.active_builds, 2);
        assert_eq!(ic.pending_builds, 3);
        assert_eq!(ic.total_workers, 5);
        assert_eq!(ic.idle_workers, 1);
        assert_eq!(ic.upload_speed_mean_mbps, Some(400.0));
        assert_eq!(ic.storage_read_mbps, Some(1_200.0));
        assert_eq!(ic.storage_write_mbps, Some(900.0));
        assert_eq!(ic.compression_ratio, Some(0.4));
    }

    #[test]
    fn the_path_coefficient_is_the_time_beyond_the_bytes() {
        let spans: Vec<(f64, f64)> = (0..200)
            .map(|i| (f64::from(i % 7) * 30.0, f64::from(i % 11)))
            .collect();
        let mut row = PathFitRow {
            n: spans.len() as f64,
            ..Default::default()
        };
        let add = |sum: &mut Option<f64>, value: f64| *sum = Some(sum.unwrap_or(0.0) + value);
        for (b, p) in spans {
            let y = 2.0 + 0.04 * b + 0.25 * p;
            add(&mut row.sb, b);
            add(&mut row.sp, p);
            add(&mut row.sy, y);
            add(&mut row.sbb, b * b);
            add(&mut row.spp, p * p);
            add(&mut row.sbp, b * p);
            add(&mut row.sby, b * y);
            add(&mut row.spy, p * y);
        }

        assert!((per_path_secs(&row).unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(
            per_path_secs(&PathFitRow { n: 10.0, ..row }),
            None,
            "too few spans"
        );
    }

    #[test]
    fn the_substitute_cost_splits_a_path_overhead_from_the_megabytes() {
        let mut row = SubstituteFitRow {
            n: 50.0,
            ..Default::default()
        };
        let add = |sum: &mut Option<f64>, value: f64| *sum = Some(sum.unwrap_or(0.0) + value);
        for i in 0..50 {
            let b = f64::from(i % 13) * 40.0;
            let y = 5.0 + 0.2 * b;
            add(&mut row.sb, b);
            add(&mut row.sy, y);
            add(&mut row.sbb, b * b);
            add(&mut row.sby, b * y);
        }

        let cost = substitute_cost(&row).unwrap();
        assert!((cost.per_path_secs - 5.0).abs() < 1e-9, "{cost:?}");
        assert!((cost.secs_per_mb - 0.2).abs() < 1e-9, "{cost:?}");
        assert_eq!(
            substitute_cost(&SubstituteFitRow { n: 10.0, ..row }),
            None,
            "too few spans"
        );
    }

    #[tokio::test]
    async fn eval_history_maps_row_into_prediction() {
        let pid = TaskId::now_v7();
        let row: BTreeMap<String, Value> = [
            ("task".to_owned(), Value::from(pid.into_inner())),
            ("p95_ram".to_owned(), Value::from(42_000.0_f64)),
            ("samples".to_owned(), Value::from(7_i64)),
        ]
        .into_iter()
        .collect();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let h = compute_eval_history(&db, gradient_types::now())
            .await
            .for_job(pid, BOTH);
        assert_eq!(h.predicted_peak_ram_mb, Some(42_000));
        assert_eq!(h.samples, 7);
    }

    const FETCH: &[FlakeStep] = &[FlakeStep::FetchFlake];
    const EVALUATE: &[FlakeStep] = &[FlakeStep::EvaluateFlake, FlakeStep::EvaluateDerivations];
    const BOTH: &[FlakeStep] = &[
        FlakeStep::FetchFlake,
        FlakeStep::EvaluateFlake,
        FlakeStep::EvaluateDerivations,
    ];

    fn durations(task: TaskId, fetch_ms: Option<f64>, evaluate_ms: Option<f64>) -> EvalDurationRow {
        EvalDurationRow {
            task,
            fetch_ms,
            fetch_samples: i64::from(fetch_ms.is_some()) * 2,
            evaluate_ms,
            evaluate_samples: i64::from(evaluate_ms.is_some()) * 2,
        }
    }

    fn expected_ms(history: &EvalHistory, task: TaskId, steps: &[FlakeStep]) -> Option<u64> {
        history.for_job(task, steps).uncontended_build_time_ms
    }

    #[test]
    fn a_job_is_estimated_by_the_fetch_and_evaluation_it_runs() {
        let task = TaskId::now_v7();
        let history = EvalHistory::from_rows(
            Vec::new(),
            vec![durations(task, Some(20_000.0), Some(300_000.0))],
        );

        assert_eq!(expected_ms(&history, task, FETCH), Some(20_000));
        assert_eq!(expected_ms(&history, task, EVALUATE), Some(300_000));
        assert_eq!(expected_ms(&history, task, BOTH), Some(320_000));
        assert!(!history.for_job(task, BOTH).from_fleet_mean);
    }

    #[test]
    fn a_task_without_runs_of_a_step_takes_the_mean_of_every_task() {
        let (seen, fetched_only, unseen) = (TaskId::now_v7(), TaskId::now_v7(), TaskId::now_v7());
        let history = EvalHistory::from_rows(
            Vec::new(),
            vec![
                durations(seen, Some(10_000.0), Some(100_000.0)),
                durations(TaskId::now_v7(), Some(30_000.0), Some(300_000.0)),
                durations(fetched_only, Some(40_000.0), None),
            ],
        );

        assert_eq!(expected_ms(&history, unseen, FETCH), Some(26_666));
        assert_eq!(expected_ms(&history, unseen, EVALUATE), Some(200_000));
        assert_eq!(expected_ms(&history, fetched_only, BOTH), Some(240_000));
        assert!(history.for_job(fetched_only, BOTH).from_fleet_mean);
        assert!(!history.for_job(fetched_only, FETCH).from_fleet_mean);
    }

    #[test]
    fn a_step_nobody_measured_leaves_the_job_unestimated() {
        let task = TaskId::now_v7();
        let history =
            EvalHistory::from_rows(Vec::new(), vec![durations(task, Some(5_000.0), None)]);

        assert_eq!(expected_ms(&history, task, BOTH), None);
        assert_eq!(expected_ms(&history, task, FETCH), Some(5_000));
    }
}
