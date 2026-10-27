/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::exec_jobs::reads::{self, ExecJobDetail, ExecJobRow};
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::exec::{DeviceRequest, ExecSpec};
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

use super::ExecJobAccessContext;
use crate::endpoints::evals::EvalAccessContext;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobAttemptView {
    pub number: i32,
    pub worker: String,
    pub started_at: NaiveDateTime,
    pub ended_at: Option<NaiveDateTime>,
    pub end_reason: Option<AttemptEndReason>,
    pub message: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobPhaseView {
    pub name: String,
    pub attempt: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobResponse {
    pub id: ExecJobId,
    pub status: ExecJobStatus,
    pub entry_point: String,
    pub evaluation: EvaluationId,
    pub task: String,
    pub project: String,
    pub nodes: u32,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub waiting_reason: Option<WaitingReason>,
    pub message: Option<String>,
    pub retries_left: i32,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub worker: Option<String>,
    pub attempts: Vec<ExecJobAttemptView>,
    pub phases: Vec<ExecJobPhaseView>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobSummary {
    pub id: ExecJobId,
    pub entry_point: String,
    pub status: ExecJobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub message: Option<String>,
    pub nodes: u32,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub started_at: Option<NaiveDateTime>,
    pub last_phase: Option<String>,
}

fn spec_of(job: &MExecJob) -> ExecSpec {
    serde_json::from_value(job.spec.clone()).unwrap_or_default()
}

fn waiting_reason_of(job: &MExecJob) -> Option<WaitingReason> {
    job.waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
}

fn response(ctx: ExecJobAccessContext) -> ExecJobResponse {
    let ExecJobDetail {
        job,
        entry_point,
        attempts,
        phases,
        ..
    } = ctx.detail;
    let spec = spec_of(&job);
    let number_of = |attempt: ExecJobAttemptId| {
        attempts
            .iter()
            .find(|a| a.id == attempt)
            .map_or(0, |a| a.number)
    };
    let phases = phases
        .into_iter()
        .map(|phase| ExecJobPhaseView {
            attempt: number_of(phase.attempt),
            name: phase.name,
            created_at: phase.created_at,
        })
        .collect();
    ExecJobResponse {
        id: job.id,
        status: job.status,
        entry_point,
        evaluation: job.evaluation,
        task: ctx.task.name,
        project: ctx.project.name,
        nodes: spec.nodes,
        labels: spec.labels,
        devices: spec.devices,
        waiting_reason: waiting_reason_of(&job),
        message: job.message,
        retries_left: job.retries_left,
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        worker: attempts.last().map(|a| a.worker.clone()),
        attempts: attempts
            .into_iter()
            .map(|a| ExecJobAttemptView {
                number: a.number,
                worker: a.worker,
                started_at: a.started_at,
                ended_at: a.ended_at,
                end_reason: a.end_reason,
                message: a.message,
            })
            .collect(),
        phases,
    }
}

fn summary(row: ExecJobRow) -> ExecJobSummary {
    let spec = spec_of(&row.job);
    ExecJobSummary {
        id: row.job.id,
        entry_point: row.entry_point,
        status: row.job.status,
        waiting_reason: waiting_reason_of(&row.job),
        message: row.job.message,
        nodes: spec.nodes,
        labels: spec.labels,
        devices: spec.devices,
        started_at: row.job.started_at,
        last_phase: row.last_phase,
    }
}

#[utoipa::path(
    get,
    path = "/exec-jobs/{job}",
    operation_id = "getExecJob",
    tag = "exec-jobs",
    summary = "Get exec job",
    description = "Returns an exec job with its status, wait reason, requested labels and devices, attempts and phases. `worker` is the worker of the latest attempt.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Exec job", body = ExecJobResponse), WebError)
)]
pub async fn get_exec_job(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<ExecJobResponse>>> {
    let ctx = ExecJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    Ok(ok_json(response(ctx)))
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/exec-jobs",
    operation_id = "listEvaluationExecJobs",
    tag = "exec-jobs",
    summary = "List exec jobs of an evaluation",
    description = "Returns every exec job of the evaluation, ordered by entry point, with its status, wait reason and last phase.\n\n**Access:** Public projects, or members of the project.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "Exec jobs", body = Vec<ExecJobSummary>), WebError)
)]
pub async fn list_evaluation_exec_jobs(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<Vec<ExecJobSummary>>>> {
    let ctx =
        EvalAccessContext::load(&state.web_db, evaluation, &maybe_user, api_key.as_ref()).await?;
    let rows = reads::of_evaluation(&state.web_db, ctx.evaluation.id).await?;
    Ok(ok_json(rows.into_iter().map(summary).collect()))
}
