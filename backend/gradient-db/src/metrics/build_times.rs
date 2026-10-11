/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::worker_names::worker_name_sql;
use chrono::NaiveDateTime;
use sea_orm::{ConnectionTrait, DbErr, QueryResult};
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize)]
pub struct ExpensiveBuild {
    pub build_id: Uuid,
    pub project: Uuid,
    pub name: String,
    pub build_time_ms: i64,
    pub worker: Option<String>,
    pub worker_name: Option<String>,
}

impl From<QueryResult> for ExpensiveBuild {
    fn from(r: QueryResult) -> Self {
        Self {
            build_id: r.try_get("", "id").unwrap_or_default(),
            project: r.try_get("", "project").unwrap_or_default(),
            name: r.try_get("", "name").unwrap_or_default(),
            build_time_ms: r.try_get("", "build_time_ms").unwrap_or(0),
            worker: r.try_get("", "worker").ok(),
            worker_name: r.try_get("", "worker_name").ok(),
        }
    }
}

pub(super) fn window_sql(window_days: i64) -> String {
    format!("(now() AT TIME ZONE 'UTC') - interval '{window_days} days'")
}

fn latest_build_metric_cte(window_days: i64) -> String {
    format!(
        "metric AS ( \
           SELECT DISTINCT ON (dm.derivation) dm.derivation, dm.build_time_ms, dm.worker_id \
           FROM derivation_metric dm \
           WHERE dm.build_time_ms IS NOT NULL AND dm.created_at >= {} \
           ORDER BY dm.derivation, dm.created_at DESC \
         )",
        window_sql(window_days)
    )
}

const BUILD_TIME_MS: &str = "coalesce(m.build_time_ms, \
     EXTRACT(EPOCH FROM (ba.build_finished_at - ba.build_started_at))::bigint * 1000)";

const BUILD_IS_TIMED: &str = "(m.derivation IS NOT NULL \
     OR (ba.build_started_at IS NOT NULL AND ba.build_finished_at IS NOT NULL))";

fn build_time_joins(shared_build: &str) -> String {
    format!(
        "LEFT JOIN metric m ON m.derivation = {shared_build}.derivation \
         LEFT JOIN LATERAL ( \
           SELECT ba2.build_started_at, ba2.build_finished_at, ba2.dispatched_job \
           FROM build_attempt ba2 \
           WHERE ba2.derivation_build = {shared_build}.derivation_build AND m.derivation IS NULL \
           ORDER BY ba2.created_at DESC LIMIT 1 \
         ) ba ON true"
    )
}

fn completed_build_clauses(window_days: i64) -> Vec<String> {
    vec![
        format!(
            "b.status = {}",
            crate::sql::status::build(gradient_entity::build::BuildStatus::Completed)
        ),
        format!("b.created_at >= {}", window_sql(window_days)),
    ]
}

fn expensive_jobs_sql(window_days: i64, project_filter: Option<&str>) -> String {
    let mut clauses = completed_build_clauses(window_days);
    if let Some(list) = project_filter {
        clauses.push(format!("pr.project IN ({list})"));
    }

    format!(
        "WITH {metric}, shared_build AS ( \
           SELECT DISTINCT ON (b.id) bj.id, pr.project, d.name, b.id AS derivation_build, b.derivation \
           FROM build_job bj \
           JOIN derivation_build b ON b.id = bj.derivation_build \
           JOIN derivation d ON d.id = b.derivation \
           JOIN evaluation ev ON ev.id = bj.evaluation \
           JOIN task pr ON pr.id = ev.task \
           WHERE {clauses} \
           ORDER BY b.id, bj.created_at DESC \
         ), ranked AS ( \
           SELECT a.id, a.project, a.name, {BUILD_TIME_MS} AS build_time_ms, \
           coalesce(m.worker_id, dj.worker_id) AS worker \
           FROM shared_build a {timing} \
           LEFT JOIN dispatched_job dj ON dj.id = ba.dispatched_job \
           WHERE {BUILD_IS_TIMED} \
           ORDER BY build_time_ms DESC LIMIT 20 \
         ) \
         SELECT r.id, r.project, r.name, r.build_time_ms, r.worker, wn.display_name AS worker_name \
         FROM ranked r {worker_name} \
         ORDER BY r.build_time_ms DESC",
        metric = latest_build_metric_cte(window_days),
        clauses = clauses.join(" AND "),
        timing = build_time_joins("a"),
        worker_name = worker_name_sql("r.worker", project_filter),
    )
}

crate::sql_fn! {
    EXPENSIVE_JOBS = || expensive_jobs_sql(30, Some("'11111111-1111-1111-1111-111111111111'")),
        params = [],
        tier = Bulk;
}

pub async fn expensive_builds<C: ConnectionTrait>(
    db: &C,
    window_days: i64,
    project_filter: Option<&str>,
) -> Result<Vec<ExpensiveBuild>, DbErr> {
    let rows = db
        .query_all_raw(
            EXPENSIVE_JOBS.bind_built(expensive_jobs_sql(window_days, project_filter), []),
        )
        .await?;

    Ok(rows.into_iter().map(ExpensiveBuild::from).collect())
}

#[derive(Serialize)]
pub struct TopProjectBuildTime {
    pub project: Uuid,
    pub project_name: String,
    pub project_display_name: String,
    pub total_build_ms: i64,
    pub build_count: i64,
}

impl From<QueryResult> for TopProjectBuildTime {
    fn from(r: QueryResult) -> Self {
        Self {
            project: r.try_get("", "project").unwrap_or_default(),
            project_name: r.try_get("", "project_name").unwrap_or_default(),
            project_display_name: r.try_get("", "project_display_name").unwrap_or_default(),
            total_build_ms: r.try_get("", "total").unwrap_or(0),
            build_count: r.try_get("", "cnt").unwrap_or(0),
        }
    }
}

fn top_projects_by_buildtime_sql(window_days: i64) -> String {
    format!(
        "WITH {metric}, shared_build AS ( \
           SELECT b.id AS derivation_build, b.derivation, named.project \
           FROM derivation_build b \
           CROSS JOIN LATERAL ( \
             SELECT DISTINCT pr.project FROM build_job bj \
             JOIN evaluation ev ON ev.id = bj.evaluation \
             JOIN task pr ON pr.id = ev.task \
             WHERE bj.derivation_build = b.id \
           ) named \
           WHERE {clauses} \
         ) \
         SELECT a.project, p.name AS project_name, p.display_name AS project_display_name, \
         sum({BUILD_TIME_MS})::bigint AS total, count(*)::bigint AS cnt \
         FROM shared_build a \
         JOIN project p ON p.id = a.project {timing} \
         WHERE {BUILD_IS_TIMED} \
         GROUP BY a.project, p.name, p.display_name ORDER BY total DESC LIMIT 15",
        metric = latest_build_metric_cte(window_days),
        clauses = completed_build_clauses(window_days).join(" AND "),
        timing = build_time_joins("a"),
    )
}

crate::sql_fn! {
    TOP_PROJECTS_BY_BUILDTIME = || top_projects_by_buildtime_sql(30),
        params = [],
        tier = Bulk;
}

pub async fn top_projects<C: ConnectionTrait>(
    db: &C,
    window_days: i64,
) -> Result<Vec<TopProjectBuildTime>, DbErr> {
    let rows = db
        .query_all_raw(
            TOP_PROJECTS_BY_BUILDTIME.bind_built(top_projects_by_buildtime_sql(window_days), []),
        )
        .await?;

    Ok(rows.into_iter().map(TopProjectBuildTime::from).collect())
}

fn board_durations_heatmap_sql(window_hours: i64, project_filter: Option<&str>) -> String {
    let scope = project_filter
        .map(|list| format!(" AND pr.project IN ({list})"))
        .unwrap_or_default();

    format!(
        "SELECT date_trunc('hour', ba.build_finished_at) AS t, \
                width_bucket((extract(epoch from (ba.build_finished_at - ba.build_started_at)) * 1000)::bigint, \
                             ARRAY[10000,30000,60000,180000,600000,1800000]::bigint[]) AS band, \
                count(*)::bigint AS c \
         FROM build_attempt ba JOIN derivation_build b ON b.id = ba.derivation_build \
         WHERE ba.build_finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours' \
           AND ba.build_started_at IS NOT NULL \
           AND b.status = {completed} \
           AND NOT EXISTS (SELECT 1 FROM build_attempt later \
                           WHERE later.derivation_build = ba.derivation_build \
                             AND (later.created_at, later.id) > (ba.created_at, ba.id)) \
           AND EXISTS (SELECT 1 FROM build_job bj \
                       JOIN evaluation ev ON ev.id = bj.evaluation \
                       JOIN task pr ON pr.id = ev.task \
                       WHERE bj.derivation_build = b.id{scope}) \
         GROUP BY t, band ORDER BY t",
        completed = crate::sql::status::build(gradient_entity::build::BuildStatus::Completed),
    )
}

crate::sql_fn! {
    BOARD_DURATIONS_HEATMAP = || board_durations_heatmap_sql(
        24,
        Some("'11111111-1111-1111-1111-111111111111'"),
    ),
        params = [],
        tier = Bulk;
}

pub struct DurationCount {
    pub hour: NaiveDateTime,
    pub band: i32,
    pub count: i64,
}

pub async fn durations_per_hour<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
    project_filter: Option<&str>,
) -> Result<Vec<DurationCount>, DbErr> {
    let rows = db
        .query_all_raw(BOARD_DURATIONS_HEATMAP.bind_built(
            board_durations_heatmap_sql(window_hours, project_filter),
            [],
        ))
        .await?;

    Ok(rows
        .iter()
        .map(|r| DurationCount {
            hour: r.try_get("", "t").unwrap_or_default(),
            band: r.try_get("", "band").unwrap_or(0),
            count: r.try_get("", "c").unwrap_or(0),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: &str = "'11111111-1111-1111-1111-111111111111'";

    #[test]
    fn expensive_jobs_are_one_row_per_derivation_build() {
        let sql = expensive_jobs_sql(30, Some(SCOPE));
        assert!(sql.contains("DISTINCT ON (b.id)"), "sql = {sql}");
        assert!(
            sql.contains(&format!("pr.project IN ({SCOPE})")),
            "sql = {sql}"
        );
    }

    #[test]
    fn expensive_jobs_prefer_the_worker_measured_build_time() {
        let sql = expensive_jobs_sql(30, None);
        assert!(sql.contains("FROM derivation_metric dm"), "sql = {sql}");
        assert!(
            sql.contains("coalesce(m.build_time_ms, EXTRACT(EPOCH FROM (ba.build_finished_at - ba.build_started_at))"),
            "sql = {sql}"
        );
    }

    #[test]
    fn expensive_jobs_name_the_worker_within_the_callers_scope() {
        let sql = expensive_jobs_sql(30, Some(SCOPE));
        assert!(sql.contains("AS worker_name"), "sql = {sql}");
        assert!(
            sql.contains(&format!("wr.peer_id IN ({SCOPE})")),
            "sql = {sql}"
        );
    }

    #[test]
    fn top_projects_carry_the_project_name() {
        assert!(top_projects_by_buildtime_sql(30).contains("p.name AS project_name"));
    }

    #[test]
    fn top_projects_count_each_build_once_per_project() {
        let sql = top_projects_by_buildtime_sql(30);
        assert!(
            sql.contains("SELECT DISTINCT pr.project FROM build_job bj")
                && sql.contains("WHERE bj.derivation_build = b.id"),
            "sql = {sql}"
        );
        assert!(sql.contains("FROM shared_build a"), "sql = {sql}");
        assert!(!sql.contains("count(*) FROM build_job"), "sql = {sql}");
    }

    #[test]
    fn top_projects_and_expensive_jobs_share_the_build_time_source() {
        for sql in [
            top_projects_by_buildtime_sql(30),
            expensive_jobs_sql(30, None),
        ] {
            assert!(sql.contains(&latest_build_metric_cte(30)), "sql = {sql}");
            assert!(sql.contains(BUILD_TIME_MS), "sql = {sql}");
        }
    }

    #[test]
    fn the_durations_heatmap_counts_each_build_once() {
        let sql = super::board_durations_heatmap_sql(24, Some("'p'"));
        assert!(
            sql.contains("FROM build_attempt ba JOIN derivation_build b"),
            "{sql}"
        );
        assert!(
            sql.contains("NOT EXISTS (SELECT 1 FROM build_attempt later"),
            "{sql}"
        );
        assert!(
            sql.contains("EXISTS (SELECT 1 FROM build_job bj")
                && sql.contains("pr.project IN ('p')"),
            "{sql}"
        );
        assert!(
            !sql.contains("FROM build_job bj JOIN derivation_build"),
            "{sql}"
        );
    }
}
