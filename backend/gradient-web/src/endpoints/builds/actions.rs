/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_project_member;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::resource::Resource;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_types::*;
use std::sync::Arc;

use super::BuildAccessContext;

async fn load_member_build(
    db: &WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    build_id: BuildJobId,
) -> WebResult<BuildAccessContext> {
    let api_key_ref = api_key.as_ref();
    let ctx = BuildAccessContext::load(db, build_id, &Some(user.clone()), api_key_ref).await?;
    if !is_project_member(db, user.id, ctx.project.id, api_key_ref).await? {
        return Err(WebError::not_found(Resource::Build));
    }

    Ok(ctx)
}

#[utoipa::path(
    post,
    path = "/builds/{build}/abort",
    operation_id = "abortBuild",
    tag = "builds",
    summary = "Abort build",
    description = "Stops a created, queued or running build of a running evaluation. The build becomes `Aborted` and its dependents in the evaluation become `DependencyFailed`. The evaluation no longer waits for them, and no heal of the evaluation queues them again. A later evaluation builds them again. Requires project membership. Answers 409 when the evaluation has finished, the build has another status, or another running evaluation still needs the build.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_build_abort(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_member_build(&state.web_db, &user, &api_key, build_id).await?;

    scheduler
        .abort_build(ctx.build_job.evaluation, ctx.shared_build.id)
        .await??;

    Ok(ok_json("Success".to_string()))
}

#[utoipa::path(
    post,
    path = "/builds/{build}/retry",
    operation_id = "retryBuild",
    tag = "builds",
    summary = "Retry build",
    description = "Queues a `FailedPermanent`, `FailedTimeout`, `Aborted` or `DependencyFailed` build again, together with its dependents in the evaluation that failed only through it. A `DependencyFailed` build retries the failed builds below it. A reproducible builder failure is retried too. A retry reopens a `Failed` or `Aborted` evaluation that reached its build phase. Requires project membership. Answers 409 when the evaluation has completed or never reached its build phase, a newer or running evaluation of the task exists, the build has another status, no failed dependency is retryable, or its worker has not confirmed an earlier abort yet.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_build_retry(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = load_member_build(&state.web_db, &user, &api_key, build_id).await?;

    scheduler
        .retry_build(ctx.build_job.evaluation, ctx.shared_build.id)
        .await??;

    Ok(ok_json("Success".to_string()))
}

#[utoipa::path(
    post,
    path = "/builds/{build}/prioritize",
    operation_id = "prioritizeBuild",
    tag = "builds",
    summary = "Prioritize build",
    description = "Hands out the build and every open build of its dependency closure ahead of unprioritized work. Requires a superuser (`403` with `code: \"superuser_required\"` otherwise). A build loses its priority when it fails permanently, dependency-fails, times out, is aborted or is skipped.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_build_prioritize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_superuser(&user)?;
    let ctx = BuildAccessContext::load_unguarded(&state.web_db, build_id).await?;

    scheduler.prioritize_build(ctx.shared_build.id).await?;

    Ok(ok_json("Success".to_string()))
}
