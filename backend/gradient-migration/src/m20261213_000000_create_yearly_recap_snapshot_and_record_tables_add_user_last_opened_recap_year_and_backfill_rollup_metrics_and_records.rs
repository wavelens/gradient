/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.yearly_build_resource_record (
    id uuid PRIMARY KEY,
    year integer NOT NULL,
    kind smallint NOT NULL,
    value bigint NOT NULL,
    derivation uuid,
    derivation_name text NOT NULL,
    derivation_build uuid,
    project uuid REFERENCES public.project(id) ON DELETE SET NULL,
    project_name text,
    project_display_name text,
    recorded_at timestamp NOT NULL,
    UNIQUE (year, kind)
);

CREATE TABLE public.yearly_instance_recap_snapshot (
    id uuid PRIMARY KEY,
    year integer NOT NULL UNIQUE,
    created_at timestamp NOT NULL,
    data_since date NOT NULL,
    statistics jsonb NOT NULL
);

CREATE TEMP TABLE minute_metric_full_history ON COMMIT DROP AS
SELECT CASE WHEN j.status = 3 THEN 'exec.completed' ELSE 'exec.failed' END AS metric,
       date_trunc('minute', j.finished_at) AS bucket_start,
       jsonb_build_object('project', t.project::text) AS scope,
       hashtextextended(t.project::text, 0) AS scope_hash,
       count(*)::bigint AS count, 0::double precision AS sum
FROM exec_job j
JOIN evaluation e ON e.id = j.evaluation
JOIN task t ON t.id = e.task
WHERE j.finished_at IS NOT NULL AND j.status IN (3, 4)
GROUP BY 1, 2, t.project
UNION ALL
SELECT 'builds.created_per_task', date_trunc('minute', b.created_at),
       jsonb_build_object('project', pr.project::text, 'task', pr.id::text),
       hashtextextended(pr.id::text, 0),
       count(DISTINCT b.id)::bigint, 0
FROM build_job bj
JOIN derivation_build b ON b.id = bj.derivation_build
JOIN evaluation ev ON ev.id = bj.evaluation
JOIN task pr ON pr.id = ev.task
GROUP BY date_trunc('minute', b.created_at), pr.id
UNION ALL
SELECT m.metric, date_trunc('minute', s.at),
       jsonb_build_object('worker', s.worker_id), hashtextextended(s.worker_id, 0),
       count(*)::bigint, sum(s.ms)
FROM (
    SELECT ws.worker_id, ws.at, ws.assigned_jobs, ws.cpu_usage_pct, ws.max_concurrent_builds,
           extract(epoch from (ws.at - lag(ws.at) OVER (PARTITION BY ws.worker_id ORDER BY ws.at))) * 1000 AS ms
    FROM worker_sample ws
) s
CROSS JOIN (VALUES ('worker.assigned_job_time_ms'), ('worker.full_load_time_ms')) m(metric)
WHERE s.ms IS NOT NULL AND s.ms <= 120000
  AND CASE m.metric
        WHEN 'worker.assigned_job_time_ms' THEN s.assigned_jobs > 0
        ELSE s.cpu_usage_pct > 90
             OR (s.max_concurrent_builds > 0 AND s.assigned_jobs >= s.max_concurrent_builds)
      END
GROUP BY m.metric, date_trunc('minute', s.at), s.worker_id;

INSERT INTO metric_rollup
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram)
SELECT uuidv7(), metric, 0, bucket_start, scope, scope_hash, count, sum, 0, 0, 0, NULL
FROM minute_metric_full_history
WHERE bucket_start >= date_trunc('day', (now() AT TIME ZONE 'UTC') - interval '2 days')
ON CONFLICT (metric, granularity, bucket_start, scope_hash) DO NOTHING;

INSERT INTO metric_rollup
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram)
SELECT uuidv7(), metric, 1, date_trunc('hour', bucket_start),
       (array_agg(scope ORDER BY bucket_start DESC))[1], scope_hash,
       sum(count)::bigint, sum(sum), 0, 0, 0, NULL
FROM minute_metric_full_history
WHERE bucket_start >= date_trunc('day', (now() AT TIME ZONE 'UTC') - interval '2 days')
GROUP BY metric, scope_hash, date_trunc('hour', bucket_start)
ON CONFLICT (metric, granularity, bucket_start, scope_hash) DO NOTHING;

INSERT INTO metric_rollup
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram)
SELECT uuidv7(), metric, 2, date_trunc('day', bucket_start),
       (array_agg(scope ORDER BY bucket_start DESC))[1], scope_hash,
       sum(count)::bigint, sum(sum), 0, 0, 0, NULL
FROM minute_metric_full_history
GROUP BY metric, scope_hash, date_trunc('day', bucket_start)
ON CONFLICT (metric, granularity, bucket_start, scope_hash) DO NOTHING;

INSERT INTO metric_rollup
    (id, metric, granularity, bucket_start, scope, scope_hash, count, sum, min, max, sum_sq, histogram)
SELECT uuidv7(), metric, 3, date_trunc('week', bucket_start),
       (array_agg(scope ORDER BY bucket_start DESC))[1], scope_hash,
       sum(count)::bigint, sum(sum), 0, 0, 0, NULL
FROM minute_metric_full_history
GROUP BY metric, scope_hash, date_trunc('week', bucket_start)
ON CONFLICT (metric, granularity, bucket_start, scope_hash) DO NOTHING;

INSERT INTO public.yearly_build_resource_record
    (id, year, kind, value, derivation, derivation_name, derivation_build, project, project_name,
     project_display_name, recorded_at)
SELECT uuidv7(), c.year, c.kind, c.value, c.derivation, c.derivation_name, p.derivation_build,
       p.project, p.name, p.display_name, c.created_at
FROM (
    SELECT DISTINCT ON (extract(year FROM dm.created_at)::integer, v.kind)
           extract(year FROM dm.created_at)::integer AS year, v.kind, v.value, dm.derivation,
           coalesce(dm.pname, '') AS derivation_name, dm.created_at
    FROM derivation_metric dm
    CROSS JOIN LATERAL (VALUES (0::smallint, dm.build_time_ms),
                               (1::smallint, dm.peak_ram_mb),
                               (2::smallint, dm.cpu_time_ms)) v(kind, value)
    WHERE v.value IS NOT NULL AND extract(month FROM dm.created_at) < 12
    ORDER BY extract(year FROM dm.created_at)::integer, v.kind, v.value DESC, dm.created_at
) c
LEFT JOIN LATERAL (
    SELECT b.id AS derivation_build, pr.id AS project, pr.name, pr.display_name
    FROM derivation_build b
    JOIN build_job bj ON bj.derivation_build = b.id
    JOIN evaluation e ON e.id = bj.evaluation
    JOIN task t ON t.id = e.task
    JOIN project pr ON pr.id = t.project
    WHERE b.derivation = c.derivation
    ORDER BY e.created_at
    LIMIT 1
) p ON TRUE;

ALTER TABLE public."user" ADD COLUMN last_opened_yearly_recap_year integer;
"#;

const DOWN: &str = "DELETE FROM metric_rollup WHERE metric IN ('exec.completed', 'exec.failed', \
                    'builds.created_per_task', 'worker.assigned_job_time_ms', 'worker.full_load_time_ms'); \
                    ALTER TABLE public.\"user\" DROP COLUMN last_opened_yearly_recap_year; \
                    DROP TABLE public.yearly_instance_recap_snapshot; \
                    DROP TABLE public.yearly_build_resource_record;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
