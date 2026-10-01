/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::NON_PR;
use super::stars::user_value;
use super::tasks::eval_status;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult};

fn rail_projects_sql() -> String {
    format!(
        "WITH viewer AS (SELECT project FROM project_user WHERE \"user\" = $1 \
            UNION SELECT s.project FROM user_project_star s JOIN project sp ON sp.id = s.project \
            WHERE s.\"user\" = $1 AND sp.public) \
        SELECT p.id, p.name, p.display_name, \
            EXISTS (SELECT 1 FROM user_project_star s WHERE s.\"user\" = $1 AND s.project = p.id) AS starred, \
            EXISTS (SELECT 1 FROM project_user pu WHERE pu.\"user\" = $1 AND pu.project = p.id) AS member, \
            (SELECT count(*) FROM evaluation e JOIN task t ON t.id = e.task LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
                WHERE t.project = p.id AND {NON_PR} AND e.created_at > now() - interval '14 days')::bigint AS recent_14d, \
            (SELECT e.status FROM evaluation e JOIN task t ON t.id = e.task LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
                WHERE t.project = p.id AND {NON_PR} ORDER BY e.created_at DESC LIMIT 1) AS status, \
            (SELECT count(*) FROM task t WHERE t.project = p.id)::bigint AS task_count \
        FROM viewer v JOIN project p ON p.id = v.project ORDER BY p.name"
    )
}

fn rail_tasks_sql() -> String {
    format!(
        "SELECT t.project, t.name, (SELECT e.status FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.task = t.id AND {NON_PR} ORDER BY e.created_at DESC LIMIT 1) AS status \
        FROM task t WHERE t.project IN (SELECT s.project FROM user_project_star s JOIN project p ON p.id = s.project \
            WHERE s.\"user\" = $1 AND (p.public OR EXISTS (SELECT 1 FROM project_user pu \
            WHERE pu.project = p.id AND pu.\"user\" = $1))) ORDER BY t.name"
    )
}

crate::sql_fn! {
    RAIL_PROJECTS = rail_projects_sql,
        params = [UserId],
        tier = Bulk;

    RAIL_TASKS = rail_tasks_sql,
        params = [UserId],
        tier = Bulk;
}

crate::sql! {
    RAIL_CACHES = "SELECT c.name, c.display_name, \
            EXISTS (SELECT 1 FROM user_cache_star s WHERE s.\"user\" = $1 AND s.cache = c.id) AS starred, \
            (SELECT count(*) FROM cached_path_signature cs WHERE cs.cache = c.id)::bigint AS nar_count \
        FROM cache c WHERE c.created_by = $1 \
            OR EXISTS (SELECT 1 FROM cache_user cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
            OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_user pu ON pu.project = pc.project \
                WHERE pc.cache = c.id AND pu.\"user\" = $1) \
            OR EXISTS (SELECT 1 FROM user_cache_star s WHERE s.\"user\" = $1 AND s.cache = c.id AND c.public) \
        ORDER BY starred DESC, c.name",
        params = [UserId],
        tier = Bulk;
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailProjectRow {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub starred: bool,
    pub member: bool,
    pub recent_14d: i64,
    pub status: Option<EvaluationStatus>,
    pub task_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailTaskRow {
    pub project: ProjectId,
    pub name: String,
    pub status: Option<EvaluationStatus>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailCacheRow {
    pub name: String,
    pub display_name: String,
    pub starred: bool,
    pub nar_count: i64,
}

fn optional_status(r: &QueryResult) -> Result<Option<EvaluationStatus>, DbErr> {
    r.try_get::<Option<i32>>("", "status")?
        .map(eval_status)
        .transpose()
}

fn rail_project_row(r: &QueryResult) -> Result<RailProjectRow, DbErr> {
    Ok(RailProjectRow {
        id: ProjectId::new(r.try_get("", "id")?),
        name: r.try_get("", "name")?,
        display_name: r.try_get("", "display_name")?,
        starred: r.try_get("", "starred")?,
        member: r.try_get("", "member")?,
        recent_14d: r.try_get("", "recent_14d")?,
        status: optional_status(r)?,
        task_count: r.try_get("", "task_count")?,
    })
}

fn rail_task_row(r: &QueryResult) -> Result<RailTaskRow, DbErr> {
    Ok(RailTaskRow {
        project: ProjectId::new(r.try_get("", "project")?),
        name: r.try_get("", "name")?,
        status: optional_status(r)?,
    })
}

fn rail_cache_row(r: &QueryResult) -> Result<RailCacheRow, DbErr> {
    Ok(RailCacheRow {
        name: r.try_get("", "name")?,
        display_name: r.try_get("", "display_name")?,
        starred: r.try_get("", "starred")?,
        nar_count: r.try_get("", "nar_count")?,
    })
}

pub async fn rail_projects<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<RailProjectRow>, DbErr> {
    db.query_all_raw(RAIL_PROJECTS.bind([user_value(user)]))
        .await?
        .iter()
        .map(rail_project_row)
        .collect()
}

pub async fn rail_tasks<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<RailTaskRow>, DbErr> {
    db.query_all_raw(RAIL_TASKS.bind([user_value(user)]))
        .await?
        .iter()
        .map(rail_task_row)
        .collect()
}

pub async fn rail_caches<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<RailCacheRow>, DbErr> {
    db.query_all_raw(RAIL_CACHES.bind([user_value(user)]))
        .await?
        .iter()
        .map(rail_cache_row)
        .collect()
}
