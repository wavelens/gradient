/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_project_member;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use std::sync::Arc;

use super::{RunnerJobAccessContext, RunnerJobRefusal};

#[utoipa::path(
    post,
    path = "/runner-jobs/{job}/abort",
    operation_id = "abortRunnerJob",
    tag = "runner-jobs",
    summary = "Abort runner job",
    description = "Stops a runner job that has not finished. The job becomes `Aborted`, its worker gets an abort for the running attempt and the job never restarts. Requires project membership or a superuser. Answers 409 when the job has already finished.",
    params(("job" = RunnerJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_runner_job_abort(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(job): Path<RunnerJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = RunnerJobAccessContext::load_unguarded(&state.web_db, job).await?;
    if !user.superuser
        && !is_project_member(&state.web_db, user.id, ctx.project.id, api_key.as_ref()).await?
    {
        return Err(WebError::not_found(Resource::RunnerJob));
    }
    if ctx.detail.job.status.is_terminal() || !scheduler.abort_runner_job(job).await? {
        return Err(RunnerJobRefusal::Finished.into());
    }

    Ok(ok_json("Success".to_string()))
}
