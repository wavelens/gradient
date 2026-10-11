/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::lookup::require;
use crate::resource::Resource;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use std::collections::HashSet;
use std::sync::Arc;

/// Anything unreachable is mapped to `404`. The endpoint must never confirm or deny a commit the
/// caller cannot see.
pub async fn get_commit(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(commit_id): Path<CommitId>,
) -> WebResult<Json<BaseResponse<MCommit>>> {
    let commit = require::<ECommit>(&state.web_db, commit_id).await?;

    let evaluations = gradient_db::evaluations::of_commit(&state.web_db, commit_id).await?;

    if evaluations.is_empty() {
        return Err(WebError::not_found(Resource::Commit));
    }

    let mut project_ids: HashSet<ProjectId> = HashSet::new();

    let task_ids: Vec<TaskId> = evaluations.iter().filter_map(|e| e.task).collect();
    if !task_ids.is_empty() {
        let tasks = gradient_db::tasks::with_ids(&state.web_db, &task_ids).await?;
        project_ids.extend(tasks.into_iter().map(|p| p.project));
    }

    if project_ids.is_empty() {
        return Err(WebError::not_found(Resource::Commit));
    }

    let project_id_vec: Vec<ProjectId> = project_ids.into_iter().collect();

    let projects = gradient_db::projects::with_ids(&state.web_db, &project_id_vec).await?;

    let any_public = projects.iter().any(|o| o.public);

    let accessible = if any_public {
        true
    } else if let Some(user) = &maybe_user {
        gradient_db::projects::access_of_user_in(&state.web_db, user.id, project_id_vec)
            .await?
            .is_some()
    } else {
        false
    };

    if !accessible {
        return Err(WebError::not_found(Resource::Commit));
    }

    Ok(Json(BaseResponse {
        error: false,
        message: commit,
    }))
}
