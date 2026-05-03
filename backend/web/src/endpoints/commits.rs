/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::helpers::OptionExt;
use crate::error::WebResult;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use uuid::Uuid;

pub async fn get_commit(
    state: State<Arc<ServerState>>,
    Extension(_user): Extension<MUser>,
    Path(commit_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<MCommit>>> {
    let commit = ECommit::find_by_id(commit_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Commit")?;

    // TODO: Check if user has access to the commit

    let res = BaseResponse {
        error: false,
        message: commit,
    };

    Ok(Json(res))
}
