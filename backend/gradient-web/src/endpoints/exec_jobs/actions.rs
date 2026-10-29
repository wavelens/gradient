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
use gradient_db::WebDb;
use gradient_types::*;
use std::sync::Arc;

use super::{ExecJobAccessContext, ExecJobRefusal};

#[utoipa::path(
    post,
    path = "/exec/{job}/abort",
    operation_id = "abortExecJob",
    tag = "exec-jobs",
    summary = "Abort exec job",
    description = "Stops an exec job that has not finished. The job becomes `Aborted`, its worker gets an abort for the running attempt and the job never restarts. Requires project membership or a superuser. Answers 409 when the job has already finished.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_exec_job_abort(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = member_context(&state.web_db, &user, &api_key, job).await?;
    if ctx.detail.status.is_terminal() || !scheduler.abort_exec_job(ctx.detail.job.id).await? {
        return Err(ExecJobRefusal::Finished.into());
    }

    Ok(ok_json("Success".to_string()))
}

#[utoipa::path(
    post,
    path = "/exec/{job}/run",
    operation_id = "runExecJob",
    tag = "exec-jobs",
    summary = "Run exec job again",
    description = "Queues a finished exec job again, every node of a multi-node job included. The next attempt gets the next number; earlier attempts, logs, phases and outputs stay. Requires project membership or a superuser. Answers 409 while the job is open, while exec jobs are disabled on the instance, project or task (the message names the switch), when the program is no longer built or when the exec spec cannot be read.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_exec_job_run(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = member_context(&state.web_db, &user, &api_key, job).await?;
    scheduler.run_exec_job(ctx.detail.job.id).await??;

    Ok(ok_json("Success".to_string()))
}

#[utoipa::path(
    post,
    path = "/exec/{job}/keep-vm-running",
    operation_id = "keepExecJobVmRunning",
    tag = "exec-jobs",
    summary = "Keep the VM of an exec job running",
    description = "Keeps the guest of an exec job running after its program exits, every node of a multi-node job included. The job stays `Running` with the program's exit code in `program_exit_code`, terminals keep working and `maxSilent` no longer applies; the overall timeout still does. The job ends when the switch is released, when it is aborted or when the guest shuts down. Works before the job starts too. Requires project membership or a superuser. Answers 409 when the job has already finished.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_exec_job_keep_vm_running(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    switch_keep_vm_running(&state.web_db, &user, &api_key, &scheduler, job, true).await
}

#[utoipa::path(
    delete,
    path = "/exec/{job}/keep-vm-running",
    operation_id = "releaseExecJobVm",
    tag = "exec-jobs",
    summary = "Let the VM of an exec job stop",
    description = "Releases the keep VM running switch. A job whose program already exited ends `Completed` or `Failed` by the recorded exit code; a job whose program still runs only loses the switch. Requires project membership or a superuser. Answers 409 when the job has already finished.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Success", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_exec_job_keep_vm_running(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<gradient_scheduler::Scheduler>>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    switch_keep_vm_running(&state.web_db, &user, &api_key, &scheduler, job, false).await
}

async fn switch_keep_vm_running(
    db: &WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    scheduler: &gradient_scheduler::Scheduler,
    job: ExecJobId,
    keep: bool,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = member_context(db, user, api_key, job).await?;
    if ctx.detail.status.is_terminal()
        || !scheduler
            .keep_exec_vm_running(ctx.detail.job.id, keep)
            .await?
    {
        return Err(ExecJobRefusal::Finished.into());
    }

    Ok(ok_json("Success".to_string()))
}

async fn member_context(
    db: &WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    job: ExecJobId,
) -> WebResult<ExecJobAccessContext> {
    let ctx = ExecJobAccessContext::load_unguarded(db, job).await?;
    if !user.superuser && !is_project_member(db, user.id, ctx.project.id, api_key.as_ref()).await? {
        return Err(WebError::not_found(Resource::ExecJob));
    }
    Ok(ctx)
}
