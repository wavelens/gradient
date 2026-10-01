/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::search::{NameHitRow, NameKind, name_hit_row};
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, Value};
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

    STARRED = concat!("SELECT 'project' AS kind, NULL::text AS project, p.name, p.display_name, true AS starred \
        FROM user_project_star s JOIN project p ON p.id = s.project WHERE s.\"user\" = $1 AND ", project_readable!(), " \
        UNION ALL SELECT 'task', p.name, t.name, t.display_name, true FROM user_task_star s \
        JOIN task t ON t.id = s.task JOIN project p ON p.id = t.project WHERE s.\"user\" = $1 AND ", project_readable!(), " \
        UNION ALL SELECT 'cache', NULL, c.name, c.display_name, true FROM user_cache_star s \
        JOIN cache c ON c.id = s.cache WHERE s.\"user\" = $1 AND ", cache_readable!(), " \
        ORDER BY kind, project, name"),
        params = [UserId],
        tier = Hot;
}

pub(super) fn user_value(user: UserId) -> Value {
    Value::Uuid(Some(user.into_inner()))
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
    db.execute_raw(query.bind([user_value(user), target.into()]))
        .await?;
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
    db.execute_raw(query.bind([user_value(user), target.into()]))
        .await?;
    Ok(())
}

pub async fn starred<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Vec<NameHitRow>, DbErr> {
    db.query_all_raw(STARRED.bind([user_value(user)]))
        .await?
        .iter()
        .map(name_hit_row)
        .collect()
}

pub async fn starred_names<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<StarredNames, DbErr> {
    let mut out = StarredNames::default();
    for row in starred(db, user).await? {
        match row.kind {
            NameKind::Project => out.projects.push(row.name),
            NameKind::Task { project } => out.tasks.push(StarredTask {
                project,
                task: row.name,
            }),
            NameKind::Cache => out.caches.push(row.name),
        }
    }
    Ok(out)
}
