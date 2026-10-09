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
use gradient_db::status::BuildRefusal;
use gradient_types::*;
use std::sync::Arc;

use super::BuildAccessContext;

async fn load_member_build(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: &MaybeApiKey,
    build_id: BuildJobId,
) -> WebResult<BuildAccessContext> {
    let api_key_ref = api_key.as_ref();
    let ctx = BuildAccessContext::load(state, build_id, &Some(user.clone()), api_key_ref).await?;
    if !is_project_member(state, user.id, ctx.project.id, api_key_ref).await? {
        return Err(WebError::not_found("Build"));
    }

    Ok(ctx)
}

fn refusal_error(refusal: BuildRefusal) -> WebError {
    match refusal {
        BuildRefusal::EvaluationFinished => {
            WebError::conflict("The evaluation of this build has already finished")
        }
        BuildRefusal::WrongStatus(status) => WebError::conflict(format!("The build is {status:?}")),
        BuildRefusal::NeededByOtherEvaluation => {
            WebError::conflict("Another running evaluation still needs this build")
        }
        BuildRefusal::WorkerStillStopping => {
            WebError::conflict("The worker has not confirmed the abort yet, try again shortly")
        }
    }
}

pub async fn post_build_abort(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_member_build(&state, &user, &api_key, build_id).await?;

    scheduler
        .abort_build(ctx.build_job.evaluation, ctx.shared_build.id)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?
        .map_err(refusal_error)?;

    Ok(ok_json("Success".to_string()))
}

pub async fn post_build_retry(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_member_build(&state, &user, &api_key, build_id).await?;

    scheduler
        .retry_build(ctx.build_job.evaluation, ctx.shared_build.id)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?
        .map_err(refusal_error)?;

    Ok(ok_json("Success".to_string()))
}

pub async fn post_build_prioritize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_member_build(&state, &user, &api_key, build_id).await?;

    scheduler
        .prioritize_build(ctx.shared_build.id)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;

    Ok(ok_json("Success".to_string()))
}
