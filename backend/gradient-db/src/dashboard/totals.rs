/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::NON_PR;
use super::stars::user_value;
use chrono::NaiveDate;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub cpu_time_ms: i64,
    pub cpu_time_ms_7d: i64,
    pub builds_completed: i64,
    pub queue_wait_p50_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ActivityDay {
    pub date: NaiveDate,
    pub evaluations: i64,
    pub failed: i64,
}

fn in_projects(column: &str, filter: Option<&str>) -> String {
    filter
        .map(|list| format!(" AND {column} IN ({list})"))
        .unwrap_or_default()
}

fn metrics_in_projects(filter: Option<&str>) -> String {
    filter
        .map(|list| {
            format!(
                " AND EXISTS (SELECT 1 FROM build_job bj JOIN evaluation e ON e.id = bj.evaluation \
                JOIN task t ON t.id = e.task WHERE bj.derivation = dm.derivation AND t.project IN ({list}))"
            )
        })
        .unwrap_or_default()
}

pub fn totals_sql(filter: Option<&str>) -> String {
    format!(
        "SELECT \
            coalesce(sum(dm.cpu_time_ms), 0)::bigint AS cpu_time_ms, \
            coalesce(sum(dm.cpu_time_ms) FILTER (WHERE dm.created_at > now() - interval '7 days'), 0)::bigint AS cpu_time_ms_7d, \
            count(*)::bigint AS builds_completed, \
            (SELECT coalesce((percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM (j.dispatched_at - j.queued_at)))) * 1000, 0)::bigint \
                FROM dispatched_job j WHERE j.dispatched_at > now() - interval '24 hours'{jobs}) AS queue_wait_p50_ms \
        FROM derivation_metric dm \
        WHERE true{metrics}",
        jobs = in_projects("j.project", filter),
        metrics = metrics_in_projects(filter),
    )
}

pub fn activity_sql(filter: Option<&str>) -> String {
    format!(
        "WITH per_day AS (SELECT e.created_at::date AS day, count(*)::bigint AS evaluations, \
            count(*) FILTER (WHERE e.status = {failed})::bigint AS failed \
            FROM evaluation e JOIN task t ON t.id = e.task LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.created_at >= current_date - 370 AND {NON_PR}{scope} GROUP BY 1) \
        SELECT d::date AS date, coalesce(p.evaluations, 0) AS evaluations, coalesce(p.failed, 0) AS failed \
        FROM generate_series(current_date - 370, current_date, interval '1 day') d \
        LEFT JOIN per_day p ON p.day = d::date ORDER BY d",
        failed = crate::sql::status::eval(EvaluationStatus::Failed),
        scope = in_projects("t.project", filter),
    )
}

crate::sql_fn! {
    TOTALS = || totals_sql(Some("'11111111-1111-1111-1111-111111111111'")),
        params = [],
        tier = Bulk;

    ACTIVITY = || activity_sql(Some("'11111111-1111-1111-1111-111111111111'")),
        params = [],
        tier = Bulk;
}

crate::sql! {
    CACHE_SIZE = "SELECT coalesce(sum(u.bytes), 0)::bigint AS bytes FROM cache_usage u \
        JOIN cache c ON c.id = u.cache \
        WHERE $2 OR c.created_by = $1 OR EXISTS (SELECT 1 FROM project_cache pc \
        JOIN project_user pu ON pu.project = pc.project WHERE pc.cache = c.id AND pu.\"user\" = $1)",
        params = [UserId, Bool(false)];
}

fn no_projects(filter: Option<&str>) -> bool {
    filter.is_some_and(str::is_empty)
}

pub async fn scoped_totals<C: ConnectionTrait>(
    db: &C,
    filter: Option<&str>,
) -> Result<Totals, DbErr> {
    if no_projects(filter) {
        return Ok(Totals::default());
    }
    let Some(r) = db
        .query_one_raw(TOTALS.bind_built(totals_sql(filter), []))
        .await?
    else {
        return Ok(Totals::default());
    };
    Ok(Totals {
        cpu_time_ms: r.try_get("", "cpu_time_ms")?,
        cpu_time_ms_7d: r.try_get("", "cpu_time_ms_7d")?,
        builds_completed: r.try_get("", "builds_completed")?,
        queue_wait_p50_ms: r.try_get("", "queue_wait_p50_ms")?,
    })
}

pub async fn cache_size<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    superuser: bool,
) -> Result<i64, DbErr> {
    let stmt = CACHE_SIZE.bind([user_value(user), superuser.into()]);
    Ok(match db.query_one_raw(stmt).await? {
        Some(r) => r.try_get("", "bytes")?,
        None => 0,
    })
}

fn activity_day(r: &QueryResult) -> Result<ActivityDay, DbErr> {
    Ok(ActivityDay {
        date: r.try_get("", "date")?,
        evaluations: r.try_get("", "evaluations")?,
        failed: r.try_get("", "failed")?,
    })
}

pub async fn activity<C: ConnectionTrait>(
    db: &C,
    filter: Option<&str>,
) -> Result<Vec<ActivityDay>, DbErr> {
    if no_projects(filter) {
        return Ok(Vec::new());
    }
    db.query_all_raw(ACTIVITY.bind_built(activity_sql(filter), []))
        .await?
        .iter()
        .map(activity_day)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[test]
    fn the_cache_size_tile_reads_the_recounted_usage() {
        let sql = CACHE_SIZE.text();
        assert!(sql.contains("FROM cache_usage u"), "{sql}");
        assert!(!sql.contains("cached_path"), "{sql}");
    }

    #[tokio::test]
    async fn an_empty_project_scope_skips_the_queries() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert_eq!(
            scoped_totals(&db, Some("")).await.unwrap(),
            Totals::default()
        );
        assert!(activity(&db, Some("")).await.unwrap().is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
