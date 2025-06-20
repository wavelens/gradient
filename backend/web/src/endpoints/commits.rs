/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::{Extension, Json};
use crate::error::{WebError, WebResult};
use core::types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use uuid::Uuid;

pub async fn get_commit(
    state: State<Arc<ServerState>>,
    Extension(_user): Extension<MUser>,
    Path(commit_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<MCommit>>> {
    let commit = ECommit::find_by_id(commit_id).one(&state.db).await?
        .ok_or_else(|| WebError::not_found("Commit"))?;

    // TODO: Check if user has access to the commit

    let res = BaseResponse {
        error: false,
        message: commit,
    };

    Ok(Json(res))
}
