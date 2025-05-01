/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use core::types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use uuid::Uuid;

pub async fn get_commit(
    state: State<Arc<ServerState>>,
    Extension(_user): Extension<MUser>,
    Path(commit_id): Path<Uuid>,
) -> Result<Json<BaseResponse<MCommit>>, (StatusCode, Json<BaseResponse<String>>)> {
    let commit = match ECommit::find_by_id(commit_id).one(&state.db).await.unwrap() {
        Some(b) => b,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Commit not found".to_string(),
                }),
            ))
        }
    };

    // TODO: Check if user has access to the commit

    let res = BaseResponse {
        error: false,
        message: commit,
    };

    Ok(Json(res))
}
