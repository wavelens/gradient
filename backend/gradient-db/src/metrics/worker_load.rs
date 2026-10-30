/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::dispatched_job::DispatchedJobKind;
use sea_orm::{ConnectionTrait, DbErr};

use super::worker_samples::workers_serving;

pub struct LoadHours {
    pub dimension: String,
    pub key: String,
    pub hours: f64,
    pub workers: i64,
}

fn since(window_hours: i64) -> String {
    format!("((now() AT TIME ZONE 'UTC') - interval '{window_hours} hours')")
}

fn busy_hours_sql(window_hours: i64, project_filter: Option<&str>) -> String {
    format!(
        "SELECT v.dimension, v.key, sum(j.hours)::float8 AS hours, count(DISTINCT j.worker_id) AS workers \
         FROM ( \
           SELECT worker_id, kind, job_context->>'architecture' AS architecture, \
                  coalesce((job_context->>'fetch_flake')::boolean, false) AS fetches, \
                  extract(epoch FROM coalesce(finished_at, now() AT TIME ZONE 'UTC') \
                                   - greatest(dispatched_at, {since})) / 3600 AS hours \
           FROM dispatched_job \
           WHERE (created_at >= {since} - interval '1 day' OR finished_at IS NULL) \
             AND (finished_at IS NULL OR finished_at > {since}){scope} \
         ) j \
         CROSS JOIN LATERAL (VALUES \
           ('capability', CASE j.kind WHEN {eval} THEN 'eval' WHEN {build} THEN 'build' END), \
           ('capability', CASE WHEN j.kind = {eval} AND j.fetches THEN 'network' END), \
           ('architecture', nullif(j.architecture, '{builtin}')) \
         ) v(dimension, key) \
         WHERE v.key IS NOT NULL AND j.hours > 0 \
         GROUP BY v.dimension, v.key",
        since = since(window_hours),
        scope = project_filter
            .map(|list| format!(" AND project IN ({list})"))
            .unwrap_or_default(),
        eval = i16::from(DispatchedJobKind::Eval),
        build = i16::from(DispatchedJobKind::Build),
        builtin = gradient_types::BUILTIN_ARCH,
    )
}

/// Hours are counted from the window start, so the first and the last hour of the window are whole.
fn capacity_hours_sql(window_hours: i64, project_filter: Option<&str>) -> String {
    format!(
        "SELECT v.dimension, v.key, sum(h.slots)::float8 AS hours, count(DISTINCT h.worker_id) AS workers \
         FROM ( \
           SELECT worker_id, max(max_concurrent_builds) AS slots, \
                  bool_or(coalesce((capabilities->>'eval')::boolean, false)) AS eval, \
                  bool_or(coalesce((capabilities->>'network')::boolean, false)) AS network, \
                  bool_or(coalesce((capabilities->>'build')::boolean, false)) AS build, \
                  (array_agg(architectures ORDER BY at DESC))[1] AS architectures \
           FROM worker_sample \
           WHERE at >= {since}{scope} \
           GROUP BY worker_id, floor(extract(epoch FROM at - {since}) / 3600) \
         ) h \
         CROSS JOIN LATERAL ( \
           SELECT 'capability', 'eval' WHERE h.eval \
           UNION ALL SELECT 'capability', 'network' WHERE h.network \
           UNION ALL SELECT 'capability', 'build' WHERE h.build \
           UNION ALL SELECT 'architecture', unnest(h.architectures) \
         ) v(dimension, key) \
         GROUP BY v.dimension, v.key",
        since = since(window_hours),
        scope = project_filter.map(workers_serving).unwrap_or_default(),
    )
}

crate::sql_fn! {
    WORKER_BUSY_HOURS = || busy_hours_sql(24, Some("'11111111-1111-1111-1111-111111111111'")),
        params = [];
    WORKER_CAPACITY_HOURS = || capacity_hours_sql(24, Some("'11111111-1111-1111-1111-111111111111'")),
        params = [];
}

async fn load_hours<C: ConnectionTrait>(
    db: &C,
    query: &'static crate::sql::Query,
    sql: String,
) -> Result<Vec<LoadHours>, DbErr> {
    let rows = db.query_all_raw(query.bind_built(sql, [])).await?;
    Ok(rows
        .iter()
        .map(|r| LoadHours {
            dimension: r.try_get("", "dimension").unwrap_or_default(),
            key: r.try_get("", "key").unwrap_or_default(),
            hours: r.try_get("", "hours").unwrap_or(0.0),
            workers: r.try_get("", "workers").unwrap_or(0),
        })
        .collect())
}

pub async fn busy_hours<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
    project_filter: Option<&str>,
) -> Result<Vec<LoadHours>, DbErr> {
    load_hours(
        db,
        &WORKER_BUSY_HOURS,
        busy_hours_sql(window_hours, project_filter),
    )
    .await
}

pub async fn capacity_hours<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
    project_filter: Option<&str>,
) -> Result<Vec<LoadHours>, DbErr> {
    load_hours(
        db,
        &WORKER_CAPACITY_HOURS,
        capacity_hours_sql(window_hours, project_filter),
    )
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn load_hours_are_scoped_to_the_projects() {
        let busy = super::busy_hours_sql(24, Some("'p'"));
        assert!(busy.contains("AND project IN ('p')"), "{busy}");
        let capacity = super::capacity_hours_sql(24, Some("'p'"));
        assert!(
            capacity.contains("worker_registration WHERE peer_id IN ('p')"),
            "{capacity}"
        );
    }
}
