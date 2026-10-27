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
use gradient_db::runner_jobs::reads::{self, RunnerJobDetail, RunnerJobRow};
use gradient_entity::runner_job::RunnerJobStatus;
use gradient_entity::runner_job_attempt::AttemptEndReason;
use gradient_types::runner::{DeviceRequest, RunnerSpec};
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

use super::RunnerJobAccessContext;
use crate::endpoints::evals::EvalAccessContext;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct RunnerJobAttemptView {
    pub number: i32,
    pub worker: String,
    pub started_at: NaiveDateTime,
    pub ended_at: Option<NaiveDateTime>,
    pub end_reason: Option<AttemptEndReason>,
    pub message: Option<String>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct RunnerJobPhaseView {
    pub name: String,
    pub attempt: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct RunnerJobResponse {
    pub id: RunnerJobId,
    pub status: RunnerJobStatus,
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
    pub attempts: Vec<RunnerJobAttemptView>,
    pub phases: Vec<RunnerJobPhaseView>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct RunnerJobSummary {
    pub id: RunnerJobId,
    pub entry_point: String,
    pub status: RunnerJobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub message: Option<String>,
    pub nodes: u32,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub started_at: Option<NaiveDateTime>,
    pub last_phase: Option<String>,
}

fn spec_of(job: &MRunnerJob) -> RunnerSpec {
    serde_json::from_value(job.spec.clone()).unwrap_or_default()
}

fn waiting_reason_of(job: &MRunnerJob) -> Option<WaitingReason> {
    job.waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
}

fn response(ctx: RunnerJobAccessContext) -> RunnerJobResponse {
    let RunnerJobDetail {
        job,
        entry_point,
        attempts,
        phases,
        ..
    } = ctx.detail;
    let spec = spec_of(&job);
    let number_of = |attempt: RunnerJobAttemptId| {
        attempts
            .iter()
            .find(|a| a.id == attempt)
            .map_or(0, |a| a.number)
    };
    let phases = phases
        .into_iter()
        .map(|phase| RunnerJobPhaseView {
            attempt: number_of(phase.attempt),
            name: phase.name,
            created_at: phase.created_at,
        })
        .collect();
    RunnerJobResponse {
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
            .map(|a| RunnerJobAttemptView {
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

fn summary(row: RunnerJobRow) -> RunnerJobSummary {
    let spec = spec_of(&row.job);
    RunnerJobSummary {
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
    path = "/runner-jobs/{job}",
    operation_id = "getRunnerJob",
    tag = "runner-jobs",
    summary = "Get runner job",
    description = "Returns a runner job with its status, wait reason, requested labels and devices, attempts and phases. `worker` is the worker of the latest attempt.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = RunnerJobId, Path)),
    responses((status = OK, description = "Runner job", body = RunnerJobResponse), WebError)
)]
pub async fn get_runner_job(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<RunnerJobId>,
) -> WebResult<Json<BaseResponse<RunnerJobResponse>>> {
    let ctx =
        RunnerJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    Ok(ok_json(response(ctx)))
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/runner-jobs",
    operation_id = "listEvaluationRunnerJobs",
    tag = "runner-jobs",
    summary = "List runner jobs of an evaluation",
    description = "Returns every runner job of the evaluation, ordered by entry point, with its status, wait reason and last phase.\n\n**Access:** Public projects, or members of the project.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "Runner jobs", body = Vec<RunnerJobSummary>), WebError)
)]
pub async fn list_evaluation_runner_jobs(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<Vec<RunnerJobSummary>>>> {
    let ctx =
        EvalAccessContext::load(&state.web_db, evaluation, &maybe_user, api_key.as_ref()).await?;
    let rows = reads::of_evaluation(&state.web_db, ctx.evaluation.id).await?;
    Ok(ok_json(rows.into_iter().map(summary).collect()))
}
