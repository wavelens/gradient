/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{
    CacheAccess, Caller, ProjectAccess, TaskAccess, load_cache, load_project, load_task,
};
use crate::audit::Audit;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_db::dashboard::{StarKind, StarredNames, star, starred_names, unstar};
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use std::sync::Arc;
use uuid::Uuid;

type StarResponse = WebResult<Json<BaseResponse<bool>>>;

#[derive(Clone, Copy)]
enum Op {
    Star,
    Unstar,
}

struct StarTarget {
    kind: StarKind,
    id: Uuid,
    owner: EventOwner,
}

impl StarTarget {
    fn action(&self, op: Op) -> Action {
        match (self.kind, op) {
            (StarKind::Project, Op::Star) => Action::ProjectStar,
            (StarKind::Project, Op::Unstar) => Action::ProjectUnstar,
            (StarKind::Task, Op::Star) => Action::TaskStar,
            (StarKind::Task, Op::Unstar) => Action::TaskUnstar,
            (StarKind::Cache, Op::Star) => Action::CacheStar,
            (StarKind::Cache, Op::Unstar) => Action::CacheUnstar,
        }
    }
}

async fn apply(
    db: &DbContext,
    user: &MUser,
    audit: &Audit,
    target: StarTarget,
    op: Op,
) -> StarResponse {
    match op {
        Op::Star => star(&db.web_db, user.id, target.kind, target.id).await?,
        Op::Unstar => unstar(&db.web_db, user.id, target.kind, target.id).await?,
    }
    audit.write(target.action(op), target.owner, None).await;
    Ok(ok_json(matches!(op, Op::Star)))
}

async fn project_target(
    db: &DbContext,
    user: &MUser,
    api_key: &MaybeApiKey,
    project: String,
) -> WebResult<StarTarget> {
    let access = ProjectAccess::Readable {
        label: Resource::Project,
    };
    let project = load_project(db, Caller::User(user), api_key.as_ref(), project, access).await?;
    Ok(StarTarget {
        kind: StarKind::Project,
        id: project.id.into_inner(),
        owner: EventOwner::project(project.id),
    })
}

async fn task_target(
    db: &DbContext,
    user: &MUser,
    api_key: &MaybeApiKey,
    project: String,
    task: String,
) -> WebResult<StarTarget> {
    let caller = Caller::User(user);
    let (project, task) = load_task(
        db,
        caller,
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;
    Ok(StarTarget {
        kind: StarKind::Task,
        id: task.id.into_inner(),
        owner: EventOwner::task(project.id, task.id),
    })
}

async fn cache_target(
    db: &DbContext,
    user: &MUser,
    api_key: &MaybeApiKey,
    cache: String,
) -> WebResult<StarTarget> {
    let caller = Caller::User(user);
    let cache = load_cache(db, caller, api_key.as_ref(), cache, CacheAccess::Readable).await?;
    Ok(StarTarget {
        kind: StarKind::Cache,
        id: cache.id.into_inner(),
        owner: EventOwner::cache(cache.id),
    })
}

#[utoipa::path(
    get,
    path = "/user/stars",
    operation_id = "getUserStars",
    tag = "user",
    summary = "List starred projects, tasks and caches",
    description = "Returns the names of everything the current user starred and can still read (the visibility of `/search`, no superuser bypass), each list sorted by name. A target the user lost access to is hidden, not unstarred.",
    responses((status = OK, description = "Starred names grouped by kind", body = StarredNames), WebError)
)]
pub async fn get_stars(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<StarredNames>>> {
    Ok(ok_json(starred_names(&state.web_db, user.id).await?))
}

#[utoipa::path(
    put,
    path = "/user/stars/projects/{project}",
    operation_id = "starProject",
    tag = "user",
    summary = "Star a project",
    description = "Idempotent. `message` is `true`, the project is starred afterwards.",
    params(("project" = String, Path)),
    responses((status = OK, description = "Project starred", body = bool, content_type = "application/json"), WebError)
)]
pub async fn put_project_star(
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> StarResponse {
    let db = state.db();
    let target = project_target(&db, &user, &api_key, project).await?;
    apply(&db, &user, &audit, target, Op::Star).await
}

#[utoipa::path(
    delete,
    path = "/user/stars/projects/{project}",
    operation_id = "unstarProject",
    tag = "user",
    summary = "Unstar a project",
    description = "Idempotent, also for a project that was never starred. `message` is `false`.",
    params(("project" = String, Path)),
    responses((status = OK, description = "Project unstarred", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_project_star(
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> StarResponse {
    let db = state.db();
    let target = project_target(&db, &user, &api_key, project).await?;
    apply(&db, &user, &audit, target, Op::Unstar).await
}

#[utoipa::path(
    put,
    path = "/user/stars/tasks/{project}/{task}",
    operation_id = "starTask",
    tag = "user",
    summary = "Star a task",
    description = "Idempotent. `message` is `true`, the task is starred afterwards.",
    params(("project" = String, Path), ("task" = String, Path)),
    responses((status = OK, description = "Task starred", body = bool, content_type = "application/json"), WebError)
)]
pub async fn put_task_star(
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> StarResponse {
    let db = state.db();
    let target = task_target(&db, &user, &api_key, project, task).await?;
    apply(&db, &user, &audit, target, Op::Star).await
}

#[utoipa::path(
    delete,
    path = "/user/stars/tasks/{project}/{task}",
    operation_id = "unstarTask",
    tag = "user",
    summary = "Unstar a task",
    description = "Idempotent, also for a task that was never starred. `message` is `false`.",
    params(("project" = String, Path), ("task" = String, Path)),
    responses((status = OK, description = "Task unstarred", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_task_star(
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> StarResponse {
    let db = state.db();
    let target = task_target(&db, &user, &api_key, project, task).await?;
    apply(&db, &user, &audit, target, Op::Unstar).await
}

#[utoipa::path(
    put,
    path = "/user/stars/caches/{cache}",
    operation_id = "starCache",
    tag = "user",
    summary = "Star a cache",
    description = "Idempotent. `message` is `true`, the cache is starred afterwards.",
    params(("cache" = String, Path)),
    responses((status = OK, description = "Cache starred", body = bool, content_type = "application/json"), WebError)
)]
pub async fn put_cache_star(
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> StarResponse {
    let db = state.db();
    let target = cache_target(&db, &user, &api_key, cache).await?;
    apply(&db, &user, &audit, target, Op::Star).await
}

#[utoipa::path(
    delete,
    path = "/user/stars/caches/{cache}",
    operation_id = "unstarCache",
    tag = "user",
    summary = "Unstar a cache",
    description = "Idempotent, also for a cache that was never starred. `message` is `false`.",
    params(("cache" = String, Path)),
    responses((status = OK, description = "Cache unstarred", body = bool, content_type = "application/json"), WebError)
)]
pub async fn delete_cache_star(
    State(state): State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> StarResponse {
    let db = state.db();
    let target = cache_target(&db, &user, &api_key, cache).await?;
    apply(&db, &user, &audit, target, Op::Unstar).await
}
