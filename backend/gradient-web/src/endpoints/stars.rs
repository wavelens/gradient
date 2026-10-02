/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{
    CacheAccess, Caller, ProjectAccess, TaskAccess, load_cache, load_project, load_task,
};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
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
    state: &ServerState,
    user: &MUser,
    info: &RequestInfo,
    target: StarTarget,
    op: Op,
) -> StarResponse {
    match op {
        Op::Star => star(&state.web_db, user.id, target.kind, target.id).await?,
        Op::Unstar => unstar(&state.web_db, user.id, target.kind, target.id).await?,
    }
    audit_record(
        state,
        Some(user.id),
        target.action(op),
        target.owner,
        info,
        None,
    )
    .await;
    Ok(ok_json(matches!(op, Op::Star)))
}

async fn project_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    project: String,
) -> WebResult<StarTarget> {
    let access = ProjectAccess::Readable { label: "Project" };
    let project =
        load_project(state, Caller::User(user), api_key.as_ref(), project, access).await?;
    Ok(StarTarget {
        kind: StarKind::Project,
        id: project.id.into_inner(),
        owner: EventOwner {
            project: Some(project.id),
            ..Default::default()
        },
    })
}

async fn task_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    project: String,
    task: String,
) -> WebResult<StarTarget> {
    let caller = Caller::User(user);
    let (project, task) = load_task(
        state,
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
        owner: EventOwner {
            project: Some(project.id),
            task: Some(task.id),
            ..Default::default()
        },
    })
}

async fn cache_target(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    cache: String,
) -> WebResult<StarTarget> {
    let caller = Caller::User(user);
    let cache = load_cache(
        state,
        caller,
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;
    Ok(StarTarget {
        kind: StarKind::Cache,
        id: cache.id.into_inner(),
        owner: EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
    })
}

pub async fn get_stars(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<StarredNames>>> {
    Ok(ok_json(starred_names(&state.web_db, user.id).await?))
}

pub async fn put_project_star(
    State(state): State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> StarResponse {
    let target = project_target(&state, &user, &api_key, project).await?;
    apply(&state, &user, &info, target, Op::Star).await
}

pub async fn delete_project_star(
    State(state): State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(project): Path<String>,
) -> StarResponse {
    let target = project_target(&state, &user, &api_key, project).await?;
    apply(&state, &user, &info, target, Op::Unstar).await
}

pub async fn put_task_star(
    State(state): State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> StarResponse {
    let target = task_target(&state, &user, &api_key, project, task).await?;
    apply(&state, &user, &info, target, Op::Star).await
}

pub async fn delete_task_star(
    State(state): State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> StarResponse {
    let target = task_target(&state, &user, &api_key, project, task).await?;
    apply(&state, &user, &info, target, Op::Unstar).await
}

pub async fn put_cache_star(
    State(state): State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> StarResponse {
    let target = cache_target(&state, &user, &api_key, cache).await?;
    apply(&state, &user, &info, target, Op::Star).await
}

pub async fn delete_cache_star(
    State(state): State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(cache): Path<String>,
) -> StarResponse {
    let target = cache_target(&state, &user, &api_key, cache).await?;
    apply(&state, &user, &info, target, Op::Unstar).await
}
