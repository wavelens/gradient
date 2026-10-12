/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::build_times::window_sql;
use crate::worker_names::worker_name_sql;
use sea_orm::{ConnectionTrait, DbErr};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct ResourceMetric {
    value_expr: &'static str,
    unit: &'static str,
}

impl ResourceMetric {
    pub fn of_build(metric: &str) -> Option<Self> {
        resource_metric_expr(metric).map(|(value_expr, unit)| Self { value_expr, unit })
    }

    pub fn of_evaluation(metric: &str) -> Option<Self> {
        eval_metric_expr(metric).map(|(value_expr, unit)| Self { value_expr, unit })
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ExpensiveResource {
    pub derivation: Uuid,
    pub project: Uuid,
    pub name: String,
    pub value: f64,
    pub unit: &'static str,
    pub worker: String,
    pub worker_name: Option<String>,
}

fn resource_metric_expr(metric: &str) -> Option<(&'static str, &'static str)> {
    Some(match metric {
        "ram" => ("dm.peak_ram_mb::double precision", "MB"),
        "cpu" => ("dm.cpu_time_ms::double precision", "ms"),
        "disk" => (
            "(coalesce(dm.disk_read_bytes,0) + coalesce(dm.disk_write_bytes,0))::double precision",
            "bytes",
        ),
        _ => return None,
    })
}

fn expensive_by_resource_sql(
    value_expr: &str,
    window_days: i64,
    project_filter: Option<&str>,
) -> String {
    let project_scope = project_filter
        .map(|list| format!(" AND pr.project IN ({list})"))
        .unwrap_or_default();

    format!(
        "WITH latest AS ( \
           SELECT DISTINCT ON (dm.derivation) dm.* \
           FROM derivation_metric dm \
           WHERE dm.created_at >= {window} \
           ORDER BY dm.derivation, dm.created_at DESC \
         ), ranked AS ( \
           SELECT dm.derivation, pro.project, d.name, {value_expr} AS value, dm.worker_id \
           FROM latest dm \
           JOIN derivation d ON d.id = dm.derivation \
           JOIN LATERAL ( \
             SELECT pr.project \
             FROM build_job bj \
             JOIN evaluation ev ON ev.id = bj.evaluation \
             JOIN task pr ON pr.id = ev.task \
             WHERE bj.derivation = dm.derivation{project_scope} \
             LIMIT 1 \
           ) pro ON true \
           WHERE {value_expr} > 0 \
           ORDER BY value DESC LIMIT 20 \
         ) \
         SELECT r.derivation, r.project, r.name, r.value, r.worker_id, wn.display_name AS worker_name \
         FROM ranked r {worker_name} \
         ORDER BY r.value DESC",
        window = window_sql(window_days),
        worker_name = worker_name_sql("r.worker_id", project_filter),
    )
}

crate::sql_fn! {
    EXPENSIVE_BY_RESOURCE = || expensive_by_resource_sql(
        "dm.peak_ram_mb::double precision",
        30,
        Some("'11111111-1111-1111-1111-111111111111'"),
    ),
        params = [],
        tier = Bulk;
}

pub async fn expensive_derivations<C: ConnectionTrait>(
    db: &C,
    metric: ResourceMetric,
    window_days: i64,
    project_filter: Option<&str>,
) -> Result<Vec<ExpensiveResource>, DbErr> {
    let rows = db
        .query_all_raw(EXPENSIVE_BY_RESOURCE.bind_built(
            expensive_by_resource_sql(metric.value_expr, window_days, project_filter),
            [],
        ))
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| ExpensiveResource {
            derivation: r.try_get("", "derivation").unwrap_or_default(),
            project: r.try_get("", "project").unwrap_or_default(),
            name: r.try_get("", "name").unwrap_or_default(),
            value: r.try_get("", "value").unwrap_or(0.0),
            unit: metric.unit,
            worker: r.try_get("", "worker_id").unwrap_or_default(),
            worker_name: r.try_get("", "worker_name").ok(),
        })
        .collect())
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ExpensiveEval {
    pub evaluation: Uuid,
    pub project: Uuid,
    pub project_name: String,
    pub project_display_name: String,
    pub task_name: String,
    pub task_display_name: String,
    pub name: String,
    pub value: f64,
    pub unit: &'static str,
    pub worker: String,
    pub worker_name: Option<String>,
}

fn eval_metric_expr(metric: &str) -> Option<(&'static str, &'static str)> {
    Some(match metric {
        "rss" => ("em.peak_rss_mb::double precision", "MB"),
        "heap" => ("em.peak_heap_mb::double precision", "MB"),
        "thunks" => ("em.total_thunks::double precision", "count"),
        "fncalls" => ("em.fn_calls::double precision", "count"),
        "alloc" => ("em.alloc_bytes::double precision", "bytes"),
        "time" => ("em.total_eval_ms::double precision", "ms"),
        _ => return None,
    })
}

fn expensive_evals_by_resource_sql(
    value_expr: &str,
    window_days: i64,
    project_filter: Option<&str>,
) -> String {
    let mut clauses = Vec::new();
    if let Some(list) = project_filter {
        clauses.push(format!("t.project IN ({list})"));
    }

    clauses.push(format!(
        "em.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window_days} days'"
    ));

    format!(
        "WITH ranked AS ( \
           SELECT em.evaluation, t.project, t.name AS task_name, t.display_name AS task_display_name, \
           ev.wildcard AS name, {value_expr} AS value, em.worker_id \
           FROM evaluation_metric em \
           JOIN evaluation ev ON ev.id = em.evaluation \
           JOIN task t ON t.id = ev.task \
           WHERE {clauses} ORDER BY value DESC LIMIT 20 \
         ) \
         SELECT r.*, p.name AS project_name, p.display_name AS project_display_name, \
         wn.display_name AS worker_name \
         FROM ranked r \
         JOIN project p ON p.id = r.project {worker_name} \
         ORDER BY r.value DESC",
        clauses = clauses.join(" AND "),
        worker_name = worker_name_sql("r.worker_id", project_filter),
    )
}

crate::sql_fn! {
    EXPENSIVE_EVALS_BY_RESOURCE = || expensive_evals_by_resource_sql(
        "em.peak_rss_mb::double precision",
        30,
        Some("'11111111-1111-1111-1111-111111111111'"),
    ),
        params = [];
}

pub async fn expensive_evaluations<C: ConnectionTrait>(
    db: &C,
    metric: ResourceMetric,
    window_days: i64,
    project_filter: Option<&str>,
) -> Result<Vec<ExpensiveEval>, DbErr> {
    let rows = db
        .query_all_raw(EXPENSIVE_EVALS_BY_RESOURCE.bind_built(
            expensive_evals_by_resource_sql(metric.value_expr, window_days, project_filter),
            [],
        ))
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| ExpensiveEval {
            evaluation: r.try_get("", "evaluation").unwrap_or_default(),
            project: r.try_get("", "project").unwrap_or_default(),
            project_name: r.try_get("", "project_name").unwrap_or_default(),
            project_display_name: r.try_get("", "project_display_name").unwrap_or_default(),
            task_name: r.try_get("", "task_name").unwrap_or_default(),
            task_display_name: r.try_get("", "task_display_name").unwrap_or_default(),
            name: r.try_get("", "name").unwrap_or_default(),
            value: r.try_get("", "value").unwrap_or(0.0),
            unit: metric.unit,
            worker: r.try_get("", "worker_id").unwrap_or_default(),
            worker_name: r.try_get("", "worker_name").ok(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: &str = "'11111111-1111-1111-1111-111111111111'";

    #[test]
    fn eval_metric_expr_unknown_is_none() {
        assert!(eval_metric_expr("bogus").is_none());
        assert!(eval_metric_expr("").is_none());
    }

    #[test]
    fn expensive_resources_skip_zero_values_for_every_metric() {
        for metric in ["ram", "cpu", "disk"] {
            let (value_expr, _) = resource_metric_expr(metric).unwrap();
            let sql = expensive_by_resource_sql(value_expr, 30, None);
            assert!(sql.contains(&format!("{value_expr} > 0")), "sql = {sql}");
        }
        assert!(resource_metric_expr("bogus").is_none());
    }

    #[test]
    fn expensive_resources_name_the_worker_within_the_callers_scope() {
        for sql in [
            expensive_by_resource_sql("dm.cpu_time_ms::double precision", 30, Some(SCOPE)),
            expensive_evals_by_resource_sql("em.total_eval_ms::double precision", 30, Some(SCOPE)),
        ] {
            assert!(sql.contains("AS worker_name"), "sql = {sql}");
            assert!(
                sql.contains(&format!("wr.peer_id IN ({SCOPE})")),
                "sql = {sql}"
            );
        }
    }

    #[test]
    fn expensive_resources_rank_the_latest_metric_per_derivation() {
        let sql = expensive_by_resource_sql("dm.cpu_time_ms::double precision", 30, None);
        let latest = sql.find("SELECT DISTINCT ON (dm.derivation)").expect(&sql);
        let ranked = sql.find("ORDER BY value DESC").expect(&sql);
        assert!(latest < ranked, "sql = {sql}");
        assert!(
            sql.contains("ORDER BY dm.derivation, dm.created_at DESC"),
            "sql = {sql}"
        );
    }
}
