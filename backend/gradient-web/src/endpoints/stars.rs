/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{
    CacheAccess, Caller, ProjectAccess, TaskAccess, load_cache, load_project, load_task,
};
use crate::authorization::MaybeApiKey;
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{StarKind, StarredNames, star, starred_names, unstar};
use gradient_types::*;
use std::sync::Arc;
use uuid::Uuid;

type StarResponse = WebResult<Json<BaseResponse<bool>>>;

#[derive(Clone, Copy)]
enum Op {
    Star,
    Unstar,
}

/// The JSON `message` is whether the target is starred afterwards; repeating either op is a no-op.
async fn apply(
    state: &ServerState,
    user: &MUser,
    kind: StarKind,
    target: Uuid,
    op: Op,
) -> StarResponse {
    match op {
        Op::Star => star(&state.web_db, user.id, kind, target).await?,
        Op::Unstar => unstar(&state.web_db, user.id, kind, target).await?,
    }
    Ok(ok_json(matches!(op, Op::Star)))
}

async fn project_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    project: String,
) -> WebResult<Uuid> {
    let access = ProjectAccess::Readable { label: "Project" };
    let project =
        load_project(state, Caller::User(user), api_key.as_ref(), project, access).await?;
    Ok(project.id.into_inner())
}

async fn task_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    project: String,
    task: String,
) -> WebResult<Uuid> {
    let caller = Caller::User(user);
    let (_, task) = load_task(
        state,
        caller,
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;
    Ok(task.id.into_inner())
}

async fn cache_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    cache: String,
) -> WebResult<Uuid> {
    let caller = Caller::User(user);
    let cache = load_cache(
        state,
        caller,
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;
    Ok(cache.id.into_inner())
}

pub async fn get_stars(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<StarredNames>>> {
    Ok(ok_json(starred_names(&state.web_db, user.id).await?))
}

pub async fn put_project_star(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> StarResponse {
    let id = project_target(&state, &user, &api_key, project).await?;
    apply(&state, &user, StarKind::Project, id, Op::Star).await
}

pub async fn delete_project_star(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> StarResponse {
    let id = project_target(&state, &user, &api_key, project).await?;
    apply(&state, &user, StarKind::Project, id, Op::Unstar).await
}

pub async fn put_task_star(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> StarResponse {
    let id = task_target(&state, &user, &api_key, project, task).await?;
    apply(&state, &user, StarKind::Task, id, Op::Star).await
}

pub async fn delete_task_star(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> StarResponse {
    let id = task_target(&state, &user, &api_key, project, task).await?;
    apply(&state, &user, StarKind::Task, id, Op::Unstar).await
}

pub async fn put_cache_star(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> StarResponse {
    let id = cache_target(&state, &user, &api_key, cache).await?;
    apply(&state, &user, StarKind::Cache, id, Op::Star).await
}

pub async fn delete_cache_star(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> StarResponse {
    let id = cache_target(&state, &user, &api_key, cache).await?;
    apply(&state, &user, StarKind::Cache, id, Op::Unstar).await
}
