/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectItemAccessRequirement};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult, require_superuser};
use crate::extractors::Path;
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use std::sync::Arc;

use super::BuildAccessContext;

#[utoipa::path(
    post,
    path = "/builds/{build}/abort",
    operation_id = "abortBuild",
    tag = "builds",
    summary = "Abort build",
    description = "Stops a created, queued or running build of a running evaluation. The build becomes `Aborted` and its dependents in the evaluation become `DependencyFailed`. The evaluation no longer waits for them, and no heal of the evaluation queues them again. A later evaluation builds them again. Callers need the `triggerEvaluation` permission on the project, or superuser access. Answers 409 when the evaluation has finished, the build has another status, or another evaluation still needs the build.",
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
    let ctx = BuildAccessContext::load(
        &state.web_db,
        build_id,
        Caller::User(&user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::CanAbortRunningWork,
    )
    .await?;

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
    description = "Requeue the `FailedPermanent`, `FailedTimeout`, `Aborted` or `DependencyFailed` builds in a running evaluation. Include dependent builds in that evaluation that failed only because of the retried build. The `DependencyFailed` builds retry the failed builds underneath them. Reproducible builder failures also receive a retry. Retries add a build attempt each. Previous attempts and their logs stay readable through `GET /builds/{build}/attempts`. Finished evaluations don't change. Use `POST /evals/{evaluation}/retry` to start a new evaluation and rerun the failed builds. The caller must have `triggerEvaluation` permission on the project. Return 409 for a finished evaluation, a build in another status, no retryable failed dependencies, or a worker that hasn't acknowledged a previous cancel yet.",
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
    let ctx = BuildAccessContext::load(
        &state.web_db,
        build_id,
        Caller::User(&user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::IncludesPermission(Permission::TriggerEvaluation),
    )
    .await?;

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
    require_superuser(&user, Permission::WriteSuperuser)?;
    let ctx = BuildAccessContext::load_unguarded(&state.web_db, build_id).await?;

    scheduler.prioritize_build(ctx.shared_build.id).await?;

    Ok(ok_json("Success".to_string()))
}
