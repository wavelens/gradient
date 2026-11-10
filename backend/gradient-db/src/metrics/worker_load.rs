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
           ('capability', CASE j.kind WHEN {eval} THEN 'eval' WHEN {build} THEN 'build' WHEN {exec} THEN 'exec' END), \
           ('capability', CASE WHEN j.kind = {eval} AND j.fetches THEN 'network' END), \
           ('architecture', nullif(nullif(j.architecture, ''), '{builtin}')) \
         ) v(dimension, key) \
         WHERE v.key IS NOT NULL AND j.hours > 0 \
         GROUP BY v.dimension, v.key",
        since = since(window_hours),
        scope = project_filter
            .map(|list| format!(" AND project IN ({list})"))
            .unwrap_or_default(),
        eval = i16::from(DispatchedJobKind::Eval),
        build = i16::from(DispatchedJobKind::Build),
        exec = i16::from(DispatchedJobKind::Exec),
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
                  bool_or(coalesce((capabilities->>'exec')::boolean, false)) AS exec, \
                  (array_agg(to_jsonb(architectures) ORDER BY at DESC))[1] AS architectures \
           FROM worker_sample \
           WHERE at >= {since}{scope} \
           GROUP BY worker_id, floor(extract(epoch FROM at - {since}) / 3600) \
         ) h \
         CROSS JOIN LATERAL ( \
           SELECT 'capability', 'eval' WHERE h.eval \
           UNION ALL SELECT 'capability', 'network' WHERE h.network \
           UNION ALL SELECT 'capability', 'build' WHERE h.build \
           UNION ALL SELECT 'capability', 'exec' WHERE h.exec \
           UNION ALL SELECT 'architecture', jsonb_array_elements_text(h.architectures) \
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
    use gradient_entity::ids::WorkerSampleId;
    use gradient_entity::worker_sample;
    use gradient_fixtures::database::TestDatabase;
    use sea_orm::{EntityTrait, IntoActiveModel};

    fn sample(worker: &str, minutes_ago: i64, architectures: &[&str]) -> worker_sample::Model {
        worker_sample::Model {
            id: WorkerSampleId::now_v7(),
            worker_id: worker.into(),
            at: chrono::Utc::now().naive_utc() - chrono::Duration::minutes(minutes_ago),
            max_concurrent_builds: 2,
            capabilities: serde_json::json!({ "build": true }),
            architectures: architectures.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn capacity_reads_empty_and_changing_architecture_lists() {
        let db = TestDatabase::create().await;
        worker_sample::Entity::insert_many(
            [
                sample("bare", 1, &[]),
                sample("moved", 2, &["x86_64-linux", "aarch64-linux"]),
                sample("moved", 1, &["x86_64-linux"]),
            ]
            .map(IntoActiveModel::into_active_model),
        )
        .exec_without_returning(&db.connection())
        .await
        .expect("the samples are written");

        let rows = super::capacity_hours(&db.connection(), 24, None)
            .await
            .expect("the capacity query runs");
        let architectures: Vec<&str> = rows
            .iter()
            .filter(|row| row.dimension == "architecture")
            .map(|row| row.key.as_str())
            .collect();

        assert_eq!(architectures, ["x86_64-linux"]);
        db.finish().await;
    }

    #[tokio::test]
    async fn busy_hours_skip_empty_and_builtin_architectures() {
        let at = chrono::Utc::now().naive_utc() - chrono::Duration::hours(2);
        let mut seed = gradient_fixtures::shape::shape();
        for architecture in ["", gradient_types::BUILTIN_ARCH, "x86_64-linux"] {
            seed = seed
                .evaluation(gradient_entity::ids::EvaluationId::now_v7())
                .eval_assignment()
                .dispatched_at(at)
                .assignment_option(|job| {
                    job.job_context = serde_json::json!({ "architecture": architecture });
                });
        }
        let db = seed.into_database().await;

        let rows = super::busy_hours(&db.connection(), 24, None)
            .await
            .expect("the busy query runs");
        let architectures: Vec<&str> = rows
            .iter()
            .filter(|row| row.dimension == "architecture")
            .map(|row| row.key.as_str())
            .collect();

        assert_eq!(architectures, ["x86_64-linux"]);
        db.finish().await;
    }

    #[tokio::test]
    async fn exec_capable_worker_samples_yield_exec_capability_row_in_capacity_hours() {
        let db = TestDatabase::create().await;
        let exec_worker = worker_sample::Model {
            capabilities: serde_json::json!({ "build": true, "exec": true }),
            ..sample("vm-host", 1, &["x86_64-linux"])
        };
        worker_sample::Entity::insert_many(
            [exec_worker, sample("builder", 1, &["x86_64-linux"])]
                .map(IntoActiveModel::into_active_model),
        )
        .exec_without_returning(&db.connection())
        .await
        .expect("the samples are written");

        let rows = super::capacity_hours(&db.connection(), 24, None)
            .await
            .expect("the capacity query runs");
        let exec = rows
            .iter()
            .find(|row| row.dimension == "capability" && row.key == "exec")
            .expect("the exec capability has capacity");

        assert_eq!((exec.hours, exec.workers), (2.0, 1));
        db.finish().await;
    }

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
