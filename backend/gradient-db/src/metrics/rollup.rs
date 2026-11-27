/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_entity::dispatched_job::DispatchedJobKind;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_util::supervision::{Pass, PassError};
use gradient_util::telemetry::metric;
use gradient_wire::types::JobPhase;
use sea_orm::{ConnectionTrait, Iterable};
use tracing::{debug, warn};

use crate::DbContext;

struct BuildCount {
    name: &'static str,
    time_col: &'static str,
    filter: String,
}

fn build_counts() -> Vec<BuildCount> {
    use gradient_entity::build::BuildStatus;
    vec![
        BuildCount {
            name: metric::BUILDS_CREATED,
            time_col: "created_at",
            filter: "TRUE".into(),
        },
        BuildCount {
            name: metric::BUILDS_DISPATCHED,
            time_col: "dispatched_at",
            filter: "TRUE".into(),
        },
        // `build.updated_at` is set on every status transition.
        // It is serving as the terminal timestamp, also for builds with no attempt row.
        BuildCount {
            name: metric::BUILDS_COMPLETED,
            time_col: "updated_at",
            filter: format!(
                "b.status = {}",
                crate::sql::status::build(BuildStatus::Completed)
            ),
        },
        BuildCount {
            name: metric::BUILDS_SUBSTITUTED,
            time_col: "updated_at",
            filter: format!(
                "b.status = {}",
                crate::sql::status::build(BuildStatus::Substituted)
            ),
        },
        BuildCount {
            name: metric::BUILDS_FAILED,
            time_col: "updated_at",
            filter: format!(
                "b.status IN ({})",
                crate::sql::status::build_in(&BuildStatus::FAILURE)
            ),
        },
    ]
}

struct BuildDuration {
    name: &'static str,
    start_col: &'static str,
    end_col: &'static str,
    filter: &'static str,
}

const BUILD_DURATIONS: &[BuildDuration] = &[BuildDuration {
    name: metric::DEPS_WAIT_MS,
    start_col: "queued_at",
    end_col: "ready_at",
    filter: "TRUE",
}];

struct EvalCount {
    name: &'static str,
    filter: String,
}

fn eval_counts() -> Vec<EvalCount> {
    use gradient_entity::evaluation::EvaluationStatus;
    vec![
        EvalCount {
            name: metric::EVALS_COMPLETED,
            filter: format!(
                "e.status = {}",
                crate::sql::status::eval(EvaluationStatus::Completed)
            ),
        },
        EvalCount {
            name: metric::EVALS_FAILED,
            filter: format!(
                "e.status = {}",
                crate::sql::status::eval(EvaluationStatus::Failed)
            ),
        },
    ]
}

const CASCADES: &[(RollupGranularity, RollupGranularity, &str)] = &[
    (
        RollupGranularity::Hour,
        RollupGranularity::Minute,
        "3 hours",
    ),
    (RollupGranularity::Day, RollupGranularity::Hour, "2 days"),
    (RollupGranularity::Week, RollupGranularity::Day, "2 weeks"),
];

const MINUTE_WINDOW: &str = "15 minutes";

struct ExecJobStatusCountMetric {
    name: &'static str,
    status: ExecJobStatus,
}

const FINISHED_EXEC_JOB_ROLLUP_METRICS: &[ExecJobStatusCountMetric] = &[
    ExecJobStatusCountMetric {
        name: metric::EXEC_COMPLETED,
        status: ExecJobStatus::Completed,
    },
    ExecJobStatusCountMetric {
        name: metric::EXEC_FAILED,
        status: ExecJobStatus::Failed,
    },
];

struct WorkerTimeRollupMetric {
    name: &'static str,
    time_counts_toward_metric_condition: &'static str,
}

const WORKER_TIME_ROLLUP_METRICS: &[WorkerTimeRollupMetric] = &[
    WorkerTimeRollupMetric {
        name: metric::WORKER_ASSIGNED_JOB_TIME_MS,
        time_counts_toward_metric_condition: "s.assigned_jobs > 0",
    },
    WorkerTimeRollupMetric {
        name: metric::WORKER_FULL_LOAD_TIME_MS,
        time_counts_toward_metric_condition: "s.cpu_usage_pct > 90 \
            OR (s.max_concurrent_builds > 0 AND s.assigned_jobs >= s.max_concurrent_builds)",
    },
];

fn max_counted_worker_sample_gap_secs(sample_interval_secs: u64) -> u64 {
    sample_interval_secs.saturating_mul(3).max(120)
}

fn build_recent_exec_job_status_count_rollup_sql(m: &ExecJobStatusCountMetric) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), '{name}', {minute}, date_trunc('minute', j.finished_at), \
                jsonb_build_object('project', t.project::text), \
                hashtextextended(t.project::text, 0), \
                count(*)::bigint, 0, 0, 0, 0, NULL \
         FROM exec_job j \
         JOIN evaluation e ON e.id = j.evaluation \
         JOIN task t ON t.id = e.task \
         WHERE j.finished_at >= date_trunc('minute', (now() AT TIME ZONE 'UTC') - interval '{window}') \
           AND j.status = {status} \
         GROUP BY date_trunc('minute', j.finished_at), t.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count",
        name = m.name,
        minute = i16::from(RollupGranularity::Minute),
        window = MINUTE_WINDOW,
        status = crate::sql::status::exec(m.status),
    )
}

fn build_builds_created_per_task_rollup_sql() -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), '{name}', {minute}, date_trunc('minute', b.created_at), \
                jsonb_build_object('project', pr.project::text, 'task', pr.id::text), \
                hashtextextended(pr.id::text, 0), \
                count(DISTINCT b.id)::bigint, 0, 0, 0, 0, NULL \
         FROM build_job bj \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         JOIN evaluation ev ON ev.id = bj.evaluation \
         JOIN task pr ON pr.id = ev.task \
         WHERE b.created_at >= date_trunc('minute', (now() AT TIME ZONE 'UTC') - interval '{window}') \
         GROUP BY date_trunc('minute', b.created_at), pr.id \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count",
        name = metric::BUILDS_CREATED_PER_TASK,
        minute = i16::from(RollupGranularity::Minute),
        window = MINUTE_WINDOW,
    )
}

fn build_worker_time_metric_rollup_sql(m: &WorkerTimeRollupMetric, max_gap_secs: u64) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), '{name}', {minute}, date_trunc('minute', s.at), \
                jsonb_build_object('worker', s.worker_id), hashtextextended(s.worker_id, 0), \
                count(*)::bigint, sum(s.ms), 0, 0, 0, NULL \
         FROM ( \
           SELECT ws.worker_id, ws.at, ws.assigned_jobs, ws.cpu_usage_pct, ws.max_concurrent_builds, \
                  extract(epoch from (ws.at - lag(ws.at) OVER (PARTITION BY ws.worker_id ORDER BY ws.at))) * 1000 AS ms \
           FROM worker_sample ws \
           WHERE ws.at >= date_trunc('minute', (now() AT TIME ZONE 'UTC') - interval '{window}') \
                          - interval '{max_gap_secs} seconds' \
         ) s \
         WHERE s.at >= date_trunc('minute', (now() AT TIME ZONE 'UTC') - interval '{window}') \
           AND s.ms IS NOT NULL AND s.ms <= {max_gap_ms} \
           AND ({condition}) \
         GROUP BY date_trunc('minute', s.at), s.worker_id \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum",
        name = m.name,
        minute = i16::from(RollupGranularity::Minute),
        window = MINUTE_WINDOW,
        max_gap_ms = max_gap_secs.saturating_mul(1000),
        condition = m.time_counts_toward_metric_condition,
    )
}

struct CacheTrafficMetric {
    name: &'static str,
    bytes: &'static str,
    nars: &'static str,
}

const CACHE_TRAFFIC: &[CacheTrafficMetric] = &[
    CacheTrafficMetric {
        name: metric::CACHE_BYTES_SENT,
        bytes: "bytes_sent",
        nars: "nar_count",
    },
    CacheTrafficMetric {
        name: metric::CACHE_BYTES_SENT_EXTERNAL,
        bytes: "external_bytes_sent",
        nars: "external_nar_count",
    },
];

fn cache_traffic_sql(m: &CacheTrafficMetric) -> String {
    let CacheTrafficMetric { name, bytes, nars } = m;
    format!(
        "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT uuidv7(), '{name}', {minute}, cm.bucket_time, \
           jsonb_build_object('cache', cm.cache::text), hashtextextended(cm.cache::text, 0), \
           sum(cm.{nars})::bigint, sum(cm.{bytes}), \
           min(cm.{bytes}), max(cm.{bytes}), sum(power(cm.{bytes}, 2)), NULL \
    FROM cache_metric cm \
    WHERE cm.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
      AND cm.{nars} > 0 \
    GROUP BY cm.bucket_time, cm.cache \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum, \
                  min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq",
        minute = i16::from(RollupGranularity::Minute)
    )
}

fn cache_storage_sql() -> String {
    format!(
        "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT uuidv7(), 'cache.bytes_added', {minute}, date_trunc('minute', cps.created_at), \
           jsonb_build_object('cache', cps.cache::text), hashtextextended(cps.cache::text, 0), \
           count(*)::bigint, sum(coalesce(cp.file_size, 0)), 0, 0, 0, NULL \
    FROM cached_path_signature cps JOIN cached_path cp ON cp.id = cps.cached_path \
    WHERE cps.created_at >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
    GROUP BY date_trunc('minute', cps.created_at), cps.cache \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum",
        minute = i16::from(RollupGranularity::Minute)
    )
}

fn upstream_latency_sql() -> String {
    format!(
        "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT uuidv7(), 'upstream.latency_ms', {minute}, um.bucket_time, \
           jsonb_build_object('upstream_url', um.upstream_url), hashtextextended(um.upstream_url, 0), \
           sum(um.request_count)::bigint, sum(um.latency_ms_sum), 0, 0, 0, NULL \
    FROM upstream_metric um \
    WHERE um.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
    GROUP BY um.bucket_time, um.upstream_url \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum",
        minute = i16::from(RollupGranularity::Minute)
    )
}

fn upstream_hits_sql() -> String {
    format!(
        "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT uuidv7(), 'upstream.narinfo_hits', {minute}, um.bucket_time, \
           jsonb_build_object('upstream_url', um.upstream_url), hashtextextended(um.upstream_url, 0), \
           sum(um.request_count)::bigint, sum(um.narinfo_hits), 0, 0, 0, NULL \
    FROM upstream_metric um \
    WHERE um.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
    GROUP BY um.bucket_time, um.upstream_url \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum",
        minute = i16::from(RollupGranularity::Minute)
    )
}

fn upstream_misses_sql() -> String {
    format!(
        "INSERT INTO metric_rollup \
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
    SELECT uuidv7(), 'upstream.narinfo_misses', {minute}, um.bucket_time, \
           jsonb_build_object('upstream_url', um.upstream_url), hashtextextended(um.upstream_url, 0), \
           sum(um.request_count)::bigint, sum(um.narinfo_misses), 0, 0, 0, NULL \
    FROM upstream_metric um \
    WHERE um.bucket_time >= (now() AT TIME ZONE 'UTC') - interval '15 minutes' \
    GROUP BY um.bucket_time, um.upstream_url \
    ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
    DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum",
        minute = i16::from(RollupGranularity::Minute)
    )
}

pub struct Rollup {
    ctx: DbContext,
    period: Duration,
}

impl Rollup {
    pub fn new(ctx: DbContext) -> Self {
        let period = Duration::from_secs(ctx.config.metrics_args.rollup_interval_secs.get());
        Self { ctx, period }
    }
}

impl Pass for Rollup {
    const NAME: &'static str = "rollup";
    const BUDGET: Duration = Duration::from_secs(300);

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        run_rollup(&self.ctx).await;
        Ok(())
    }
}

pub(crate) async fn run_rollup(ctx: &DbContext) {
    let db = &ctx.worker_db;
    for m in build_counts() {
        if let Err(e) = db.execute_unprepared(&build_count_sql(&m)).await {
            warn!(metric = m.name, error = %e, "rollup build-count failed");
        }
    }

    if let Err(e) = db
        .execute_unprepared(&build_builds_created_per_task_rollup_sql())
        .await
    {
        warn!(metric = metric::BUILDS_CREATED_PER_TASK, error = %e, "rollup build-count failed");
    }

    for m in FINISHED_EXEC_JOB_ROLLUP_METRICS {
        if let Err(e) = db
            .execute_unprepared(&build_recent_exec_job_status_count_rollup_sql(m))
            .await
        {
            warn!(metric = m.name, error = %e, "rollup exec-count failed");
        }
    }

    let max_gap_secs = max_counted_worker_sample_gap_secs(
        ctx.config.metrics_args.worker_sample_interval_secs.get(),
    );
    for m in WORKER_TIME_ROLLUP_METRICS {
        if let Err(e) = db
            .execute_unprepared(&build_worker_time_metric_rollup_sql(m, max_gap_secs))
            .await
        {
            warn!(metric = m.name, error = %e, "rollup worker-time failed");
        }
    }

    for m in BUILD_DURATIONS {
        if let Err(e) = db.execute_unprepared(&build_duration_sql(m)).await {
            warn!(metric = m.name, error = %e, "rollup build-duration failed");
        }
    }

    if let Err(e) = db.execute_unprepared(&build_duration_attempt_sql()).await {
        warn!(metric = metric::BUILDS_DURATION_MS, error = %e, "rollup build-duration failed");
    }

    if let Err(e) = db.execute_unprepared(&dispatch_wait_sql()).await {
        warn!(metric = metric::DISPATCH_WAIT_MS, error = %e, "rollup dispatch-wait failed");
    }

    if let Err(e) = db.execute_unprepared(&phase_duration_sql()).await {
        warn!(metric = "phase.*.ms", error = %e, "rollup phase-duration failed");
    }

    for m in eval_counts() {
        if let Err(e) = db.execute_unprepared(&eval_count_sql(&m)).await {
            warn!(metric = m.name, error = %e, "rollup eval-count failed");
        }
    }

    for m in CACHE_TRAFFIC {
        if let Err(e) = db.execute_unprepared(&cache_traffic_sql(m)).await {
            warn!(metric = m.name, error = %e, "rollup cache-traffic failed");
        }
    }

    if let Err(e) = db.execute_unprepared(&cache_storage_sql()).await {
        warn!(error = %e, "rollup cache-storage failed");
    }

    for (sql, label) in [
        (upstream_latency_sql(), metric::UPSTREAM_LATENCY_MS),
        (upstream_hits_sql(), metric::UPSTREAM_NARINFO_HITS),
        (upstream_misses_sql(), metric::UPSTREAM_NARINFO_MISSES),
    ] {
        if let Err(e) = db.execute_unprepared(&sql).await {
            warn!(metric = label, error = %e, "rollup upstream metric failed");
        }
    }

    for (target, source, window) in CASCADES {
        if let Err(e) = db
            .execute_unprepared(&cascade_sql(*target, *source, window))
            .await
        {
            warn!(target = target.trunc_unit(), error = %e, "rollup cascade failed");
        }
    }

    debug!("rollup pass complete");
}

fn build_count_sql(m: &BuildCount) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), '{name}', 0, date_trunc('minute', b.{col}), \
                jsonb_build_object('project', pr.project::text), \
                hashtextextended(pr.project::text, 0), \
                count(DISTINCT b.id)::bigint, 0, 0, 0, 0, NULL \
         FROM build_job bj \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         JOIN evaluation ev ON ev.id = bj.evaluation \
         JOIN task pr ON pr.id = ev.task \
         WHERE b.{col} IS NOT NULL \
           AND b.{col} >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ({filter}) \
         GROUP BY date_trunc('minute', b.{col}), pr.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count",
        name = m.name,
        col = m.time_col,
        window = MINUTE_WINDOW,
        filter = m.filter,
    )
}

fn build_duration_sql(m: &BuildDuration) -> String {
    let ms = format!(
        "extract(epoch from (b.{} - b.{})) * 1000",
        m.end_col, m.start_col
    );
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), '{name}', 0, date_trunc('minute', b.{end}), \
                jsonb_build_object('project', pr.project::text), \
                hashtextextended(pr.project::text, 0), \
                count(*)::bigint, sum({ms}), min({ms}), max({ms}), sum(power({ms}, 2)), NULL \
         FROM build_job bj \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         JOIN evaluation ev ON ev.id = bj.evaluation \
         JOIN task pr ON pr.id = ev.task \
         WHERE b.{end} IS NOT NULL AND b.{start} IS NOT NULL \
           AND b.{end} >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ({filter}) \
         GROUP BY date_trunc('minute', b.{end}), pr.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq",
        name = m.name,
        end = m.end_col,
        start = m.start_col,
        window = MINUTE_WINDOW,
        filter = m.filter,
        ms = ms,
    )
}

fn build_duration_attempt_sql() -> String {
    let ms = "extract(epoch from (ba.build_finished_at - ba.build_started_at)) * 1000";
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), 'builds.duration_ms', 0, date_trunc('minute', ba.build_finished_at), \
                jsonb_build_object('project', pr.project::text), \
                hashtextextended(pr.project::text, 0), \
                count(*)::bigint, sum({ms}), min({ms}), max({ms}), sum(power({ms}, 2)), NULL \
         FROM build_attempt ba \
         JOIN derivation_build b ON b.id = ba.derivation_build AND b.status = {completed} \
         JOIN build_job bj ON bj.derivation_build = b.id \
         JOIN evaluation ev ON ev.id = bj.evaluation \
         JOIN task pr ON pr.id = ev.task \
         WHERE ba.build_finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ba.build_started_at IS NOT NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM build_attempt later \
             WHERE later.derivation_build = ba.derivation_build \
               AND (later.created_at > ba.created_at \
                    OR (later.created_at = ba.created_at AND later.id > ba.id))) \
         GROUP BY date_trunc('minute', ba.build_finished_at), pr.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq",
        ms = ms,
        window = MINUTE_WINDOW,
        completed = crate::sql::status::build(gradient_entity::build::BuildStatus::Completed),
    )
}

fn dispatch_wait_sql() -> String {
    let ms = "extract(epoch from (dj.dispatched_at - dj.ready_at)) * 1000";
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), 'dispatch.wait_ms', {minute}, date_trunc('minute', dj.dispatched_at), \
                jsonb_build_object('project', dj.project::text), \
                hashtextextended(dj.project::text, 0), \
                count(*)::bigint, sum({ms}), min({ms}), max({ms}), sum(power({ms}, 2)), NULL \
         FROM dispatched_job dj \
         WHERE dj.kind = {build} \
           AND dj.ready_at IS NOT NULL \
           AND dj.dispatched_at >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
         GROUP BY date_trunc('minute', dj.dispatched_at), dj.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq",
        minute = i16::from(RollupGranularity::Minute),
        build = i16::from(DispatchedJobKind::Build),
        window = MINUTE_WINDOW,
    )
}

fn dispatched_job_kind_series_segment(kind: DispatchedJobKind) -> &'static str {
    match kind {
        DispatchedJobKind::Eval => "eval",
        DispatchedJobKind::Build => "build",
        DispatchedJobKind::Exec => "exec",
    }
}

fn phase_duration_sql() -> String {
    let last = JobPhase::ALL.iter().map(|p| p.as_i16()).max().unwrap_or(0);
    let names = (0..=last)
        .map(|v| format!("'{}'", JobPhase::name_of(v)))
        .collect::<Vec<_>>()
        .join(",");
    let kinds = DispatchedJobKind::iter()
        .map(|k| {
            format!(
                "WHEN {} THEN '{}'",
                i16::from(k),
                dispatched_job_kind_series_segment(k)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), s.metric, {minute}, s.bucket_start, \
                jsonb_build_object('project', s.project::text), \
                hashtextextended(s.project::text, 0), \
                count(*)::bigint, sum(s.ms), min(s.ms), max(s.ms), sum(power(s.ms, 2)), NULL \
         FROM ( \
           SELECT 'phase.' || CASE dj.kind {kinds} ELSE 'unknown_' || dj.kind END || '.' || \
                    (ARRAY[{names}])[p.phase + 1] || '.ms' AS metric, \
                  date_trunc('minute', p.created_at) AS bucket_start, \
                  dj.project, \
                  (p.end_ms - p.start_ms)::double precision AS ms \
           FROM dispatched_job_phase p \
           JOIN dispatched_job dj ON dj.id = p.dispatched_job \
           WHERE p.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
             AND p.phase BETWEEN 0 AND {last} \
         ) s \
         GROUP BY s.metric, s.bucket_start, s.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq",
        minute = i16::from(RollupGranularity::Minute),
        window = MINUTE_WINDOW,
        kinds = kinds,
        names = names,
        last = last,
    )
}

fn eval_count_sql(m: &EvalCount) -> String {
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), '{name}', 0, date_trunc('minute', e.finished_at), \
                jsonb_build_object('project', p.project::text), \
                hashtextextended(p.project::text, 0), \
                count(*)::bigint, 0, 0, 0, 0, NULL \
         FROM evaluation e JOIN task p ON p.id = e.task \
         WHERE e.finished_at IS NOT NULL \
           AND e.finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window}' \
           AND ({filter}) \
         GROUP BY date_trunc('minute', e.finished_at), p.project \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count",
        name = m.name,
        window = MINUTE_WINDOW,
        filter = m.filter,
    )
}

/// The grouping key must be exactly the unique index
/// `(metric, granularity, bucket_start, scope_hash)`.
/// Grouping by `scope` too would make Postgres reject the whole upsert (#571).
fn cascade_sql(target: RollupGranularity, source: RollupGranularity, window: &str) -> String {
    let unit = target.trunc_unit();
    let target = i16::from(target);
    let source = i16::from(source);
    format!(
        "INSERT INTO metric_rollup \
         (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram) \
         SELECT uuidv7(), metric, {target}, date_trunc('{unit}', bucket_start), \
                (array_agg(scope ORDER BY bucket_start DESC))[1], scope_hash, \
                sum(count)::bigint, sum(sum), min(min), max(max), sum(sum_sq), NULL \
         FROM metric_rollup \
         WHERE granularity = {source} \
           AND bucket_start >= date_trunc('{unit}', (now() AT TIME ZONE 'UTC') - interval '{window}') \
         GROUP BY metric, scope_hash, date_trunc('{unit}', bucket_start) \
         ON CONFLICT (metric, granularity, bucket_start, scope_hash) \
         DO UPDATE SET scope = EXCLUDED.scope, count = EXCLUDED.count, sum = EXCLUDED.sum, \
                       min = EXCLUDED.min, max = EXCLUDED.max, sum_sq = EXCLUDED.sum_sq"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_build_count_counts_each_shared_build_once_for_multiple_evaluations() {
        use gradient_entity::evaluation::EvaluationStatus;
        use gradient_types::EvaluationId;
        use sea_orm::ConnectionTrait;

        const A: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
        let first = EvaluationId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(first)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(A)
            .job()
            .shared_build_option(|b| b.created_at = chrono::Utc::now().naive_utc())
            .evaluation(EvaluationId::now_v7())
            .same_task_as(first)
            .derivation(A)
            .job()
            .into_database()
            .await;
        let created = build_counts()
            .into_iter()
            .find(|m| m.name == metric::BUILDS_CREATED)
            .expect("builds created is counted");

        db.connection()
            .execute_unprepared(&build_count_sql(&created))
            .await
            .expect("the build count rollup runs");

        let counts: Vec<i64> = db
            .rows::<gradient_entity::metric_rollup::Entity>()
            .await
            .into_iter()
            .filter(|row| row.metric == metric::BUILDS_CREATED)
            .map(|row| row.count)
            .collect();
        assert_eq!(counts, [1]);
        db.finish().await;
    }

    #[tokio::test]
    async fn test_exec_count_rollup_counts_each_finished_exec_job_once_by_final_status() {
        use gradient_entity::exec_job;
        use gradient_entity::ids::ExecJobId;
        use gradient_types::EvaluationId;

        let evaluation = EvaluationId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(evaluation)
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv")
            .entry_point("pkg.a")
            .into_database()
            .await;
        let conn = db.connection();
        let point = db
            .entry_point(evaluation, "pkg.a")
            .await
            .expect("the entry point");
        let now = gradient_types::now();
        let statuses = [
            ExecJobStatus::Completed,
            ExecJobStatus::Completed,
            ExecJobStatus::Failed,
            ExecJobStatus::Aborted,
            ExecJobStatus::Queued,
        ];
        for (node_index, status) in (0_i16..).zip(statuses) {
            crate::rows::insert::<exec_job::Entity, _>(
                &conn,
                exec_job::Model {
                    id: ExecJobId::now_v7(),
                    evaluation,
                    entry_point: point.id,
                    program_derivation: point.derivation,
                    requirement: serde_json::json!({}),
                    spec: serde_json::json!({}),
                    status,
                    created_at: now,
                    finished_at: status.is_terminal().then_some(now),
                    node_index,
                    ..Default::default()
                },
            )
            .await
            .expect("the exec job is written");
        }

        for m in FINISHED_EXEC_JOB_ROLLUP_METRICS {
            conn.execute_unprepared(&build_recent_exec_job_status_count_rollup_sql(m))
                .await
                .expect("the exec count rollup runs");
        }

        let mut counts: Vec<_> = db
            .rows::<gradient_entity::metric_rollup::Entity>()
            .await
            .into_iter()
            .map(|row| (row.metric, row.count))
            .collect();
        counts.sort();
        assert_eq!(
            counts,
            [
                (metric::EXEC_COMPLETED.to_owned(), 2),
                (metric::EXEC_FAILED.to_owned(), 1)
            ]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn test_worker_time_rollup_ignores_intervals_longer_than_allowed_sample_gap_and_starts_at_whole_minute()
     {
        use chrono::{Duration as ChronoDuration, DurationRound, Timelike};
        use gradient_entity::worker_sample;
        use gradient_types::WorkerSampleId;
        use sea_orm::{ActiveModelTrait, IntoActiveModel};

        if chrono::Utc::now().second() >= 50 {
            tokio::time::sleep(Duration::from_secs(11)).await;
        }
        let window_start = (gradient_types::now() - ChronoDuration::minutes(15))
            .duration_trunc(ChronoDuration::minutes(1))
            .expect("a minute truncates");
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        for offset_secs in [-60, 30, 630] {
            worker_sample::Model {
                id: WorkerSampleId::now_v7(),
                worker_id: "w1".into(),
                at: window_start + ChronoDuration::seconds(offset_secs),
                cpu_usage_pct: Some(10.0),
                assigned_jobs: 1,
                max_concurrent_builds: 4,
                capabilities: serde_json::json!({}),
                ..Default::default()
            }
            .into_active_model()
            .insert(&conn)
            .await
            .expect("the sample is written");
        }

        for m in WORKER_TIME_ROLLUP_METRICS {
            conn.execute_unprepared(&build_worker_time_metric_rollup_sql(m, 120))
                .await
                .expect("the worker time rollup runs");
        }

        let rows: Vec<_> = db
            .rows::<gradient_entity::metric_rollup::Entity>()
            .await
            .into_iter()
            .map(|row| (row.metric, row.bucket_start, row.count, row.sum))
            .collect();
        assert_eq!(
            rows,
            [(
                metric::WORKER_ASSIGNED_JOB_TIME_MS.to_owned(),
                window_start,
                1,
                90_000.0
            )]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn test_build_and_exec_phase_durations_rollup_writes_separate_rows_per_kind() {
        use gradient_types::EvaluationId;

        let phase = JobPhase::ALL[0];
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .eval_assignment()
            .assignment_option(|a| {
                a.kind = DispatchedJobKind::Build;
                a.job_id = Some(a.id.to_string());
            })
            .phase(phase.as_i16(), 0, 100)
            .eval_assignment()
            .assignment_option(|a| {
                a.kind = DispatchedJobKind::Exec;
                a.job_id = Some(a.id.to_string());
            })
            .phase(phase.as_i16(), 0, 300)
            .eval_assignment()
            .assignment_option(|a| {
                a.kind = DispatchedJobKind::Exec;
                a.job_id = Some(a.id.to_string());
            })
            .phase(phase.as_i16(), 0, 500)
            .into_database()
            .await;
        let conn = db.connection();
        conn.execute_unprepared(
            "UPDATE dispatched_job_phase SET created_at = (now() AT TIME ZONE 'UTC')",
        )
        .await
        .expect("the phases move into the rollup window");

        conn.execute_unprepared(&phase_duration_sql())
            .await
            .expect("the phase duration rollup runs");

        let mut rows: Vec<_> = db
            .rows::<gradient_entity::metric_rollup::Entity>()
            .await
            .into_iter()
            .map(|row| (row.metric, row.count, row.sum))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let name = phase.as_str();
        assert_eq!(
            rows,
            [
                (format!("phase.build.{name}.ms"), 1, 100.0),
                (format!("phase.exec.{name}.ms"), 2, 800.0),
            ]
        );
        db.finish().await;
    }
    #[tokio::test]
    async fn dispatch_wait_measures_each_dispatch_from_its_own_readiness() {
        use chrono::Duration as ChronoDuration;
        use gradient_types::EvaluationId;

        let at = gradient_types::now();
        let build = |wait: Option<i64>| {
            move |a: &mut gradient_entity::dispatched_job::Model| {
                a.kind = DispatchedJobKind::Build;
                a.job_id = Some(a.id.to_string());
                a.dispatched_at = at;
                a.ready_at = wait.map(|ms| at - ChronoDuration::milliseconds(ms));
            }
        };
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .eval_assignment()
            .assignment_option(build(Some(1000)))
            .eval_assignment()
            .assignment_option(build(Some(3000)))
            .eval_assignment()
            .assignment_option(build(None))
            .into_database()
            .await;

        db.connection()
            .execute_unprepared(&dispatch_wait_sql())
            .await
            .expect("the dispatch wait rollup runs");

        let rows: Vec<_> = db
            .rows::<gradient_entity::metric_rollup::Entity>()
            .await
            .into_iter()
            .map(|row| (row.metric, row.count, row.sum, row.min, row.max))
            .collect();
        db.finish().await;
        assert_eq!(
            rows,
            [("dispatch.wait_ms".to_owned(), 2, 4000.0, 1000.0, 3000.0)]
        );
    }

    #[tokio::test]
    async fn rollup_cascade_rebuilds_coarser_buckets_and_refreshes_newest_scope_on_rerun() {
        use gradient_entity::metric_rollup;
        use gradient_fixtures::database::TestDatabase;

        let db = TestDatabase::create().await;
        let conn = db.connection();
        let hour_start = "date_trunc('hour', (now() AT TIME ZONE 'UTC') - interval '3 hours')";
        let minute_row = |offset: &str, scope: &str| {
            format!(
                "INSERT INTO metric_rollup (id, metric, granularity, bucket_start, scope, \
                 scope_hash, count, sum, min, max, sum_sq) \
                 VALUES (uuidv7(), 'test.metric', 0, {hour_start} + interval '{offset}', \
                 '{scope}'::jsonb, 1, 2, 10, 1, 4, 0)"
            )
        };
        for sql in [
            minute_row("0 minutes", r#"{"v":"old"}"#),
            minute_row("30 minutes", r#"{"v":"new"}"#),
            minute_row("-1 minutes", r#"{"v":"outside"}"#),
        ] {
            db.execute(&sql).await;
        }
        let cascade = cascade_sql(
            RollupGranularity::Hour,
            RollupGranularity::Minute,
            "3 hours",
        );
        let hours = || async {
            db.rows::<metric_rollup::Entity>()
                .await
                .into_iter()
                .filter(|row| row.granularity == RollupGranularity::Hour)
                .map(|row| (row.count, row.sum, row.scope))
                .collect::<Vec<_>>()
        };

        conn.execute_unprepared(&cascade)
            .await
            .expect("the cascade runs");
        let first = hours().await;
        db.execute(&format!(
            "UPDATE metric_rollup SET scope = '{{\"v\":\"newer\"}}' \
             WHERE granularity = 0 AND bucket_start = {hour_start} + interval '30 minutes'"
        ))
        .await;
        conn.execute_unprepared(&cascade)
            .await
            .expect("the cascade reruns");
        let second = hours().await;

        db.finish().await;
        assert_eq!(first, [(4, 20.0, serde_json::json!({"v": "new"}))]);
        assert_eq!(second, [(4, 20.0, serde_json::json!({"v": "newer"}))]);
    }

    #[tokio::test]
    async fn build_rollups_uses_task_project_latest_finished_attempts_and_refreshes_stale_scope() {
        use gradient_entity::build::BuildStatus;
        use gradient_entity::build_attempt::AttemptOutcome;
        use gradient_entity::ids::BuildAttemptId;
        use gradient_types::EvaluationId;

        const DONE: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-done.drv";
        const UNSTARTED: &str = "/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-unstarted.drv";
        const OLD: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-old.drv";
        let now = gradient_types::now();
        let (mine, other) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let completed = move |b: &mut gradient_entity::derivation_build::Model, created| {
            b.status = BuildStatus::Completed;
            b.created_at = created;
            b.updated_at = created;
        };
        let db = gradient_fixtures::shape::shape()
            .evaluation(mine)
            .derivation(DONE)
            .job()
            .shared_build_option(|b| completed(b, now))
            .attempt(AttemptOutcome::Built, None)
            .attempt(AttemptOutcome::Built, None)
            .derivation(UNSTARTED)
            .job()
            .shared_build_option(|b| completed(b, now))
            .attempt(AttemptOutcome::Built, None)
            .derivation(OLD)
            .job()
            .shared_build_option(|b| completed(b, now - chrono::Duration::minutes(30)))
            .attempt(AttemptOutcome::Built, None)
            .evaluation(other)
            .derivation(DONE)
            .job()
            .into_database()
            .await;
        let place = |id: BuildAttemptId, minutes_ago: i64, seconds: Option<i64>| {
            let started = seconds
                .map(|s| {
                    format!("(now() AT TIME ZONE 'UTC') - interval '{minutes_ago} minutes' - interval '{s} seconds'")
                })
                .unwrap_or_else(|| "NULL".into());
            format!(
                "UPDATE build_attempt SET \
                 build_finished_at = (now() AT TIME ZONE 'UTC') - interval '{minutes_ago} minutes', \
                 build_started_at = {started} WHERE id = '{}'",
                id.into_inner()
            )
        };
        let retried = db.attempts(DONE).await;
        for (attempt, minutes_ago, seconds) in [
            (retried[0].id, 5, Some(10)),
            (retried[1].id, 2, Some(30)),
            (db.attempts(UNSTARTED).await[0].id, 1, None),
            (db.attempts(OLD).await[0].id, 20, Some(30)),
        ] {
            db.execute(&place(attempt, minutes_ago, seconds)).await;
        }
        let conn = db.connection();
        let run_all = || async {
            for m in build_counts() {
                conn.execute_unprepared(&build_count_sql(&m))
                    .await
                    .expect("the build count runs");
            }
            for m in BUILD_DURATIONS {
                conn.execute_unprepared(&build_duration_sql(m))
                    .await
                    .expect("the build duration runs");
            }
            conn.execute_unprepared(&build_duration_attempt_sql())
                .await
                .expect("the attempt duration runs");
        };
        let rows = || async { db.rows::<gradient_entity::metric_rollup::Entity>().await };
        let (mine_project, other_project) = (
            db.project_of(mine).await.to_string(),
            db.project_of(other).await.to_string(),
        );

        run_all().await;
        let first = rows().await;
        db.execute("UPDATE metric_rollup SET scope = '{\"stale\":true}'")
            .await;
        run_all().await;
        let second = rows().await;

        db.finish().await;
        let summarize = |rows: &[gradient_entity::metric_rollup::Model], metric: &str| {
            let mut found: Vec<_> = rows
                .iter()
                .filter(|row| row.metric == metric)
                .map(|row| {
                    (
                        row.scope["project"].as_str().unwrap_or_default().to_owned(),
                        row.count,
                        row.sum,
                    )
                })
                .collect();
            found.sort_by(|a, b| a.partial_cmp(b).expect("ordered"));
            found
        };
        let mut durations = vec![
            (mine_project.clone(), 1, 30_000.0),
            (other_project.clone(), 1, 30_000.0),
        ];
        durations.sort_by(|a, b| a.partial_cmp(b).expect("ordered"));
        let mut created = vec![(mine_project, 2, 0.0), (other_project, 1, 0.0)];
        created.sort_by(|a, b| a.partial_cmp(b).expect("ordered"));
        assert_eq!(summarize(&first, metric::BUILDS_DURATION_MS), durations);
        assert_eq!(summarize(&first, metric::BUILDS_CREATED), created);
        assert!(
            second.iter().all(|row| row.scope.get("project").is_some()),
            "{second:?}"
        );
        assert_eq!(first.len(), second.len());
    }

    #[tokio::test]
    async fn every_job_phase_reported_by_workers_gets_own_rollup_series_row() {
        use gradient_types::EvaluationId;

        let mut seed = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .eval_assignment()
            .assignment_option(|a| {
                a.kind = DispatchedJobKind::Build;
                a.job_id = Some(a.id.to_string());
            });
        for phase in JobPhase::ALL {
            seed = seed.phase(phase.as_i16(), 0, 100);
        }
        let db = seed.into_database().await;
        db.execute("UPDATE dispatched_job_phase SET created_at = (now() AT TIME ZONE 'UTC')")
            .await;

        db.connection()
            .execute_unprepared(&phase_duration_sql())
            .await
            .expect("the phase duration rollup runs");

        let mut names: Vec<_> = db
            .rows::<gradient_entity::metric_rollup::Entity>()
            .await
            .into_iter()
            .map(|row| row.metric)
            .collect();
        names.sort();
        db.finish().await;
        let mut expected: Vec<_> = JobPhase::ALL
            .iter()
            .map(|p| format!("phase.build.{}.ms", p.as_str()))
            .collect();
        expected.sort();
        assert_eq!(names, expected);
    }
}
