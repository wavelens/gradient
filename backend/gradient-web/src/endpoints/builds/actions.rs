/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_project_member;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use std::sync::Arc;

use super::BuildAccessContext;

pub async fn post_build_prioritize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key_ref = api_key.as_ref();
    let ctx = BuildAccessContext::load(&state, build_id, &Some(user.clone()), api_key_ref).await?;
    if !is_project_member(&state, user.id, ctx.project.id, api_key_ref).await? {
        return Err(WebError::not_found("Build"));
    }

    scheduler
        .prioritize_build(ctx.anchor.id)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;

    Ok(ok_json("Success".to_string()))
}
