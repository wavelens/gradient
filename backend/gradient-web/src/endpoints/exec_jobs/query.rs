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
use gradient_db::exec_jobs::outputs;
use gradient_db::exec_jobs::reads::{
    self, ExecJobDetail, ExecJobRow, ExecNodeDetail, StartedEvaluation,
};
use gradient_db::exec_jobs::tokens::JobTokenRow;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::exec::{DeviceRequest, ExecSpec, GuestSize};
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

use super::ExecJobAccessContext;
use super::terminal::{ssh_commands, ssh_host};
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
pub struct ExecJobNode {
    pub node: u32,
    pub id: ExecJobId,
    pub status: ExecJobStatus,
    pub worker: Option<String>,
    pub guest_address: Option<String>,
    pub master_port: Option<u16>,
    pub attempts: Vec<ExecJobAttemptView>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobPhaseView {
    pub name: String,
    pub attempt: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobTokenView {
    pub attempt: i32,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub revoked: bool,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct StartedEvaluationView {
    pub evaluation: EvaluationId,
    pub task: String,
    pub status: EvaluationStatus,
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
    pub guest: GuestSize,
    pub waiting_reason: Option<WaitingReason>,
    pub message: Option<String>,
    pub retries_left: i32,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub worker: Option<String>,
    pub attempts: Vec<ExecJobAttemptView>,
    pub node_jobs: Vec<ExecJobNode>,
    pub phases: Vec<ExecJobPhaseView>,
    pub tokens: Vec<ExecJobTokenView>,
    pub started_evaluations: Vec<StartedEvaluationView>,
    pub outputs_size: i64,
    pub ssh_command: Option<String>,
    pub forward_commands: Vec<String>,
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

fn ssh_access(
    config: &RuntimeConfig,
    project: &str,
    job: ExecJobId,
    spec: &ExecSpec,
) -> (Option<String>, Vec<String>) {
    let host =
        ssh_host(&config.server.serve_url).filter(|_| config.ssh.enable && spec.ssh != Some(false));
    match host {
        Some(host) => {
            let (shell, forwards) = ssh_commands(&host, config.ssh.port, project, job, &spec.ports);
            (Some(shell), forwards)
        }
        None => (None, Vec::new()),
    }
}

pub(crate) fn attempt_view(attempt: &MExecJobAttempt) -> ExecJobAttemptView {
    ExecJobAttemptView {
        number: attempt.number,
        worker: attempt.worker.clone(),
        started_at: attempt.started_at,
        ended_at: attempt.ended_at,
        end_reason: attempt.end_reason,
        message: attempt.message.clone(),
    }
}

fn placed_attempt(job: &MExecJob, attempts: &[MExecJobAttempt]) -> Option<MExecJobAttempt> {
    attempts
        .last()
        .filter(|_| !matches!(job.status, ExecJobStatus::Created | ExecJobStatus::Queued))
        .cloned()
}

fn node_view(node: &ExecNodeDetail) -> ExecJobNode {
    let placed = placed_attempt(&node.job, &node.attempts);
    ExecJobNode {
        node: u32::try_from(node.job.node_index).unwrap_or_default(),
        id: node.job.id,
        status: node.job.status,
        worker: placed.as_ref().map(|a| a.worker.clone()),
        guest_address: placed.as_ref().and_then(|a| a.guest_address.clone()),
        master_port: placed
            .as_ref()
            .and_then(|a| a.master_port)
            .and_then(|port| u16::try_from(port).ok()),
        attempts: node.attempts.iter().map(attempt_view).collect(),
    }
}

fn token_view(row: JobTokenRow) -> ExecJobTokenView {
    ExecJobTokenView {
        attempt: row.attempt_number,
        created_at: row.created_at,
        expires_at: row.expires_at,
        revoked: row.revoked,
    }
}

fn started_evaluation_view(row: StartedEvaluation) -> StartedEvaluationView {
    StartedEvaluationView {
        evaluation: row.evaluation,
        task: row.task,
        status: row.status,
        created_at: row.created_at,
    }
}

fn response(
    ctx: ExecJobAccessContext,
    outputs_size: i64,
    config: &RuntimeConfig,
) -> ExecJobResponse {
    let ExecJobDetail {
        job,
        status,
        entry_point,
        attempts,
        phases,
        tokens,
        started_evaluations,
        nodes,
        ..
    } = ctx.detail;
    let spec = spec_of(&job);
    let (ssh_command, forward_commands) = ssh_access(config, &ctx.project.name, job.id, &spec);
    let worker = placed_attempt(&job, &attempts).map(|a| a.worker);
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
        status,
        entry_point,
        evaluation: job.evaluation,
        task: ctx.task.name,
        project: ctx.project.name,
        nodes: spec.nodes,
        labels: spec.labels,
        devices: spec.devices,
        guest: spec.guest,
        waiting_reason: waiting_reason_of(&job),
        message: job.message,
        retries_left: job.retries_left,
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        worker,
        attempts: attempts.iter().map(attempt_view).collect(),
        node_jobs: nodes.iter().map(node_view).collect(),
        phases,
        tokens: tokens.into_iter().map(token_view).collect(),
        started_evaluations: started_evaluations
            .into_iter()
            .map(started_evaluation_view)
            .collect(),
        outputs_size,
        ssh_command,
        forward_commands,
    }
}

fn summary(row: ExecJobRow) -> ExecJobSummary {
    let spec = spec_of(&row.job);
    ExecJobSummary {
        id: row.job.id,
        entry_point: row.entry_point,
        status: row.status,
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
    path = "/exec/{job}",
    operation_id = "getExecJob",
    tag = "exec-jobs",
    summary = "Get exec job",
    description = "Returns an exec job with its status, wait reason, requested labels, devices and guest size, attempts, phases, job tokens (never their value), the evaluations it started and the total size of its outputs. Any node id of a multi-node job answers the job: `id`, the attempts, phases, tokens and outputs are node 0's, `status` follows every node and `node_jobs` lists each node with its status, worker, guest address, master port and attempts. `worker` is the worker of the latest attempt and empty while the job waits to start. `ssh_command` and `forward_commands` hold the `ssh` login and one `ssh -N -L` command per declared port while the SSH server is enabled and the job does not set `ssh = false`.\n\n**Access:** Public projects, or members of the project.",
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
    let outputs_size = outputs::of_job(&state.web_db, ctx.detail.job.id)
        .await?
        .iter()
        .map(|output| output.size)
        .sum();
    Ok(ok_json(response(ctx, outputs_size, &state.config)))
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/exec",
    operation_id = "listEvaluationExecJobs",
    tag = "exec-jobs",
    summary = "List exec jobs of an evaluation",
    description = "Returns every exec job of the evaluation, ordered by entry point, with its status, wait reason and last phase. A multi-node job is listed once, under node 0's id, with the status of all its nodes.\n\n**Access:** Public projects, or members of the project.",
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
