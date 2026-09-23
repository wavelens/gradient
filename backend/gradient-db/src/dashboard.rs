/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Value};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarKind {
    Project,
    Task,
    Cache,
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct StarredTask {
    pub project: String,
    pub task: String,
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct StarredNames {
    pub projects: Vec<String>,
    pub tasks: Vec<StarredTask>,
    pub caches: Vec<String>,
}

crate::sql! {
    STAR_PROJECT = "INSERT INTO user_project_star (\"user\", project) VALUES ($1, $2) \
        ON CONFLICT DO NOTHING",
        params = [UserId, ProjectId],
        tier = Hot;

    STAR_TASK = "INSERT INTO user_task_star (\"user\", task) VALUES ($1, $2) \
        ON CONFLICT DO NOTHING",
        params = [UserId, TaskId],
        tier = Hot;

    STAR_CACHE = "INSERT INTO user_cache_star (\"user\", cache) VALUES ($1, $2) \
        ON CONFLICT DO NOTHING",
        params = [UserId, CacheId],
        tier = Hot;

    UNSTAR_PROJECT = "DELETE FROM user_project_star WHERE \"user\" = $1 AND project = $2",
        params = [UserId, ProjectId],
        tier = Hot;

    UNSTAR_TASK = "DELETE FROM user_task_star WHERE \"user\" = $1 AND task = $2",
        params = [UserId, TaskId],
        tier = Hot;

    UNSTAR_CACHE = "DELETE FROM user_cache_star WHERE \"user\" = $1 AND cache = $2",
        params = [UserId, CacheId],
        tier = Hot;

    STARRED_PROJECTS = "SELECT p.name FROM user_project_star s JOIN project p ON p.id = s.project \
        WHERE s.\"user\" = $1 AND ($2 OR p.public OR EXISTS (SELECT 1 FROM project_user pu \
        WHERE pu.project = p.id AND pu.\"user\" = $1)) ORDER BY p.name",
        params = [UserId, Bool(false)],
        tier = Hot;

    STARRED_TASKS = "SELECT p.name AS project, t.name FROM user_task_star s \
        JOIN task t ON t.id = s.task JOIN project p ON p.id = t.project \
        WHERE s.\"user\" = $1 AND ($2 OR p.public OR EXISTS (SELECT 1 FROM project_user pu \
        WHERE pu.project = p.id AND pu.\"user\" = $1)) ORDER BY p.name, t.name",
        params = [UserId, Bool(false)],
        tier = Hot;

    STARRED_CACHES = "SELECT c.name FROM user_cache_star s JOIN cache c ON c.id = s.cache \
        WHERE s.\"user\" = $1 AND ($2 OR c.public OR c.created_by = $1 \
        OR EXISTS (SELECT 1 FROM cache_user cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
        OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_user pu ON pu.project = pc.project \
        WHERE pc.cache = c.id AND pu.\"user\" = $1)) ORDER BY c.name",
        params = [UserId, Bool(false)],
        tier = Hot;
}

fn pair(user: UserId, target: Uuid) -> [Value; 2] {
    [user.into_inner().into(), target.into()]
}

pub async fn star<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    kind: StarKind,
    target: Uuid,
) -> Result<(), DbErr> {
    let query = match kind {
        StarKind::Project => &STAR_PROJECT,
        StarKind::Task => &STAR_TASK,
        StarKind::Cache => &STAR_CACHE,
    };
    db.execute_raw(query.bind(pair(user, target))).await?;
    Ok(())
}

pub async fn unstar<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    kind: StarKind,
    target: Uuid,
) -> Result<(), DbErr> {
    let query = match kind {
        StarKind::Project => &UNSTAR_PROJECT,
        StarKind::Task => &UNSTAR_TASK,
        StarKind::Cache => &UNSTAR_CACHE,
    };
    db.execute_raw(query.bind(pair(user, target))).await?;
    Ok(())
}

fn names(rows: &[QueryResult]) -> Result<Vec<String>, DbErr> {
    rows.iter().map(|r| r.try_get("", "name")).collect()
}

fn starred_task(row: &QueryResult) -> Result<StarredTask, DbErr> {
    Ok(StarredTask {
        project: row.try_get("", "project")?,
        task: row.try_get("", "name")?,
    })
}

/// Only targets the user can still read: losing access hides a star, it does not delete it.
pub async fn starred_names<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    superuser: bool,
) -> Result<StarredNames, DbErr> {
    let bind = || -> [Value; 2] { [user.into_inner().into(), superuser.into()] };
    let projects = names(&db.query_all_raw(STARRED_PROJECTS.bind(bind())).await?)?;
    let tasks = db
        .query_all_raw(STARRED_TASKS.bind(bind()))
        .await?
        .iter()
        .map(starred_task)
        .collect::<Result<_, _>>()?;
    let caches = names(&db.query_all_raw(STARRED_CACHES.bind(bind())).await?)?;
    Ok(StarredNames {
        projects,
        tasks,
        caches,
    })
}
