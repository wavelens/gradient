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
    pub project_slug: String,
    pub evaluation: Uuid,
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
           SELECT dm.derivation, pro.project, pro.evaluation, d.name, {value_expr} AS value, dm.worker_id \
           FROM latest dm \
           JOIN derivation d ON d.id = dm.derivation \
           JOIN LATERAL ( \
             SELECT pr.project, bj.evaluation \
             FROM build_job bj \
             JOIN evaluation ev ON ev.id = bj.evaluation \
             JOIN task pr ON pr.id = ev.task \
             WHERE bj.derivation = dm.derivation{project_scope} \
             ORDER BY bj.created_at DESC \
             LIMIT 1 \
           ) pro ON true \
           WHERE {value_expr} > 0 \
           ORDER BY value DESC LIMIT 20 \
         ) \
         SELECT r.derivation, r.project, p.name AS project_slug, r.evaluation, r.name, r.value, \
         r.worker_id, wn.display_name AS worker_name \
         FROM ranked r \
         JOIN project p ON p.id = r.project {worker_name} \
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
            project_slug: r.try_get("", "project_slug").unwrap_or_default(),
            evaluation: r.try_get("", "evaluation").unwrap_or_default(),
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

    #[test]
    fn eval_metric_expr_unknown_is_none() {
        assert!(eval_metric_expr("bogus").is_none());
        assert!(eval_metric_expr("").is_none());
    }

    #[tokio::test]
    async fn expensive_derivation_ranking_skips_zero_ram_cpu_disk_values() {
        use gradient_fixtures::shape::shape;
        use gradient_types::EvaluationId;

        let now = chrono::Utc::now().naive_utc();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-heavy.drv")
            .job()
            .derivation_metric(|m| {
                m.peak_ram_mb = Some(500);
                m.cpu_time_ms = Some(7_000);
                m.disk_read_bytes = Some(3_000);
                m.disk_write_bytes = Some(1_096);
                m.created_at = now;
            })
            .derivation("/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-idle.drv")
            .job()
            .derivation_metric(|m| {
                m.peak_ram_mb = Some(0);
                m.created_at = now;
            })
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cpu-only.drv")
            .job()
            .derivation_metric(|m| {
                m.cpu_time_ms = Some(100);
                m.created_at = now;
            })
            .into_database()
            .await;
        let conn = db.connection();

        let mut ranked = Vec::new();
        for metric in ["ram", "cpu", "disk"] {
            let metric = ResourceMetric::of_build(metric).expect("a build metric");
            let rows = expensive_derivations(&conn, metric, 30, None)
                .await
                .expect("the expensive derivations read");
            ranked.push(
                rows.into_iter()
                    .map(|r| (r.name, r.value))
                    .collect::<Vec<_>>(),
            );
        }

        db.finish().await;
        assert_eq!(
            ranked,
            [
                vec![("heavy".to_owned(), 500.0)],
                vec![
                    ("heavy".to_owned(), 7_000.0),
                    ("cpu-only".to_owned(), 100.0)
                ],
                vec![("heavy".to_owned(), 4_096.0)],
            ]
        );
        assert!(ResourceMetric::of_build("bogus").is_none());
    }

    #[tokio::test]
    async fn expensive_derivations_and_evaluations_only_list_callers_projects_and_name_workers_from_registrations_in_those_projects()
     {
        use gradient_fixtures::shape::shape;
        use gradient_types::EvaluationId;

        let now = chrono::Utc::now().naive_utc();
        let (mine, other) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = shape()
            .evaluation(mine)
            .worker_registration("w1")
            .derivation("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-mine.drv")
            .job()
            .derivation_metric(|m| {
                m.peak_ram_mb = Some(100);
                m.created_at = now;
                m.worker_id = "w1".into();
            })
            .evaluation_metric(|m| {
                m.peak_rss_mb = 100;
                m.created_at = now;
                m.worker_id = "w1".into();
            })
            .evaluation(other)
            .worker_registration("w1")
            .derivation("/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-other.drv")
            .job()
            .derivation_metric(|m| {
                m.peak_ram_mb = Some(200);
                m.created_at = now;
                m.worker_id = "w1".into();
            })
            .evaluation_metric(|m| {
                m.peak_rss_mb = 200;
                m.created_at = now;
                m.worker_id = "w1".into();
            })
            .into_database()
            .await;
        let mine_project = db.project_of(mine).await.into_inner();
        let other_project = db.project_of(other).await.into_inner();
        for (project, name, created) in [
            (mine_project, "Mine", "2020-01-01"),
            (other_project, "Other", "2021-01-01"),
        ] {
            db.execute(&format!(
                "UPDATE worker_registration SET display_name = '{name}', created_at = '{created}' \
                 WHERE peer_id = '{project}'"
            ))
            .await;
        }
        let conn = db.connection();
        let scope = format!("'{mine_project}'");
        let ram = ResourceMetric::of_build("ram").expect("a build metric");
        let rss = ResourceMetric::of_evaluation("rss").expect("an evaluation metric");

        let derivations = expensive_derivations(&conn, ram, 30, Some(&scope))
            .await
            .expect("the scoped derivations");
        let evaluations = expensive_evaluations(&conn, rss, 30, Some(&scope))
            .await
            .expect("the scoped evaluations");
        let everyone = expensive_derivations(&conn, ram, 30, None)
            .await
            .expect("the unscoped derivations");

        db.finish().await;
        let derivations: Vec<_> = derivations
            .into_iter()
            .map(|r| (r.name, r.worker_name))
            .collect();
        let evaluations: Vec<_> = evaluations
            .into_iter()
            .map(|r| (r.evaluation, r.worker_name))
            .collect();
        assert_eq!(derivations, [("mine".to_owned(), Some("Mine".to_owned()))]);
        assert_eq!(evaluations, [(mine.into_inner(), Some("Mine".to_owned()))]);
        assert_eq!(
            everyone
                .into_iter()
                .map(|r| r.worker_name)
                .collect::<Vec<_>>(),
            [Some("Other".to_owned()), Some("Other".to_owned())]
        );
    }

    #[tokio::test]
    async fn expensive_resources_rank_the_latest_metric_and_link_the_newest_evaluation() {
        use gradient_fixtures::shape::shape;
        use gradient_types::EvaluationId;

        const HEAVY: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-heavy.drv";
        let now = chrono::Utc::now().naive_utc();
        let (older, newer) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = shape()
            .evaluation(older)
            .derivation(HEAVY)
            .job()
            .job_option(|j| j.created_at = now - chrono::Duration::hours(2))
            .derivation_metric(|m| {
                m.peak_ram_mb = Some(900);
                m.created_at = now - chrono::Duration::hours(2);
            })
            .derivation_metric(|m| {
                m.peak_ram_mb = Some(300);
                m.created_at = now;
            })
            .evaluation(newer)
            .derivation(HEAVY)
            .job()
            .job_option(|j| j.created_at = now - chrono::Duration::hours(1))
            .into_database()
            .await;
        let newer_task = db
            .rows::<gradient_entity::evaluation::Entity>()
            .await
            .into_iter()
            .find(|e| e.id == newer)
            .and_then(|e| e.task)
            .expect("the newer evaluation has a task");
        let project_of_newer = db
            .rows::<gradient_entity::task::Entity>()
            .await
            .into_iter()
            .find(|t| t.id == newer_task)
            .expect("the task is written")
            .project;
        let ram = ResourceMetric::of_build("ram").expect("ram is a build metric");

        let rows = expensive_derivations(&db.connection(), ram, 30, None)
            .await
            .expect("the expensive derivations read");

        let read: Vec<_> = rows
            .iter()
            .map(|r| (r.evaluation, r.project, r.value))
            .collect();
        assert_eq!(
            read,
            [(newer.into_inner(), project_of_newer.into_inner(), 300.0)]
        );
        db.finish().await;
    }
}
