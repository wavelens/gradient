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
use gradient_db::scheduling::cluster::clusters_of_assignments;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::exec::{DeviceRequest, ExecSpec, GuestSize};
use gradient_types::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

use super::ExecJobAccessContext;
use super::terminal::{ssh_commands, ssh_host};
use super::timeline::worker_timeline;
use crate::endpoints::board::JobPhaseView;
use crate::endpoints::board_subjects::WorkerNames;
use crate::endpoints::evals::EvalAccessContext;
use crate::metrics_scope::MetricsScope;

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
    pub worker_name: Option<String>,
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
    pub autostart: bool,
    pub keep_vm_running: bool,
    pub program_exit_code: Option<i32>,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub worker: Option<String>,
    pub worker_name: Option<String>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub cluster_job: Option<ClusterJobId>,
    pub attempts: Vec<ExecJobAttemptView>,
    pub node_jobs: Vec<ExecJobNode>,
    pub phases: Vec<ExecJobPhaseView>,
    pub timeline: Vec<JobPhaseView>,
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
    pub autostart: bool,
    pub keep_vm_running: bool,
    pub program_exit_code: Option<i32>,
    pub started_at: Option<NaiveDateTime>,
    pub last_phase: Option<String>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub cluster_job: Option<ClusterJobId>,
}

fn spec_of(job: &MExecJob) -> ExecSpec {
    serde_json::from_value(job.spec.clone()).unwrap_or_default()
}

fn waiting_reason_of(job: &MExecJob) -> Option<WaitingReason> {
    if job.waits_for_manual_start && job.status == ExecJobStatus::Created {
        return Some(WaitingReason::ManualStart);
    }
    job.waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
}

fn ssh_access(
    config: &RuntimeConfig,
    project: &str,
    job: &MExecJob,
    spec: &ExecSpec,
) -> (Option<String>, Vec<String>) {
    let usable =
        config.ssh.enable && spec.ssh != Some(false) && job.status == ExecJobStatus::Running;
    match ssh_host(&config.server.serve_url).filter(|_| usable) {
        Some(host) => {
            let (shell, forwards) =
                ssh_commands(&host, config.ssh.port, project, job.id, &spec.ports);
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

fn node_view(node: &ExecNodeDetail, names: &WorkerNames) -> ExecJobNode {
    let placed = placed_attempt(&node.job, &node.attempts);
    ExecJobNode {
        node: u32::try_from(node.job.node_index).unwrap_or_default(),
        id: node.job.id,
        status: node.job.status,
        worker: placed.as_ref().map(|a| a.worker.clone()),
        worker_name: placed.as_ref().and_then(|a| names.name(&a.worker)),
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

fn attempt_workers(detail: &ExecJobDetail) -> impl Iterator<Item = String> + '_ {
    detail
        .attempts
        .iter()
        .chain(detail.nodes.iter().flat_map(|node| &node.attempts))
        .map(|attempt| attempt.worker.clone())
}

fn response(
    ctx: ExecJobAccessContext,
    timeline: Vec<JobPhaseView>,
    cluster_job: Option<ClusterJobId>,
    outputs_size: i64,
    names: &WorkerNames,
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
    let (ssh_command, forward_commands) = ssh_access(config, &ctx.project.name, &job, &spec);
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
        autostart: !spec.manual_start,
        keep_vm_running: job.keep_vm_running,
        program_exit_code: job.program_exit_code,
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        worker_name: worker.as_deref().and_then(|w| names.name(w)),
        worker,
        dispatched_job: attempts.last().map(|a| a.assignment_id),
        cluster_job,
        attempts: attempts.iter().map(attempt_view).collect(),
        node_jobs: nodes.iter().map(|node| node_view(node, names)).collect(),
        phases,
        timeline,
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

fn summary(row: ExecJobRow, clusters: &HashMap<DispatchedJobId, ClusterJobId>) -> ExecJobSummary {
    let spec = spec_of(&row.job);
    ExecJobSummary {
        cluster_job: row
            .last_dispatched_job
            .and_then(|job| clusters.get(&job))
            .copied(),
        id: row.job.id,
        entry_point: row.entry_point,
        status: row.status,
        waiting_reason: waiting_reason_of(&row.job),
        message: row.job.message,
        nodes: spec.nodes,
        labels: spec.labels,
        devices: spec.devices,
        autostart: !spec.manual_start,
        keep_vm_running: row.job.keep_vm_running,
        program_exit_code: row.job.program_exit_code,
        started_at: row.job.started_at,
        last_phase: row.last_phase,
        dispatched_job: row.last_dispatched_job,
    }
}

#[utoipa::path(
    get,
    path = "/exec/{job}",
    operation_id = "getExecJob",
    tag = "exec-jobs",
    summary = "Get exec job",
    description = "Returns an exec job with its status, wait reason, requested labels, devices and guest size, attempts, phases, job tokens (never their value), the evaluations it started and the total size of its outputs. Any node id of a multi-node job answers the job: `id`, the attempts, phases, tokens and outputs are node 0's, `status` follows every node and `node_jobs` lists each node with its status, worker, guest address, master port and attempts. `worker` is the worker of the latest attempt and empty while the job waits to start and `worker_name` its display name when the caller can see the worker. `dispatched_job` is the job board assignment of the latest attempt and `cluster_job` the cluster job of a multi-node job. `timeline` holds the worker timeline of the latest attempt, each reported phase as a span lasting until the next phase or the end of the attempt. `ssh_command` and `forward_commands` hold the `ssh` login and one `ssh -N -L` command per declared port while the job is `running`, the SSH server is enabled and the job does not set `ssh = false`. `keep_vm_running` tells whether the guest stays up after the program exits, `program_exit_code` holds the exit code of a program whose guest is kept running.\n\n**Access:** Public projects, or members of the project.",
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
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let names = WorkerNames::load(&state.web_db, &scope, attempt_workers(&ctx.detail)).await?;
    let latest = ctx.detail.attempts.last();
    let timeline = match latest {
        Some(attempt) => worker_timeline(&state.web_db, attempt, &ctx.detail.phases).await,
        None => Vec::new(),
    };
    let cluster_job = match latest {
        Some(attempt) => clusters_of_assignments(&state.web_db, &[attempt.assignment_id])
            .await?
            .into_values()
            .next(),
        None => None,
    };
    Ok(ok_json(response(
        ctx,
        timeline,
        cluster_job,
        outputs_size,
        &names,
        &state.config,
    )))
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/exec",
    operation_id = "listEvaluationExecJobs",
    tag = "exec-jobs",
    summary = "List exec jobs of an evaluation",
    description = "Returns every exec job of the evaluation, ordered by entry point, with its status, wait reason, last phase, the job board assignment of its latest attempt and the cluster job of a multi-node job. A multi-node job is listed once, under node 0's id, with the status of all its nodes.\n\n**Access:** Public projects, or members of the project.",
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
    let jobs: Vec<DispatchedJobId> = rows.iter().filter_map(|r| r.last_dispatched_job).collect();
    let clusters = clusters_of_assignments(&state.web_db, &jobs).await?;
    Ok(ok_json(
        rows.into_iter()
            .map(|row| summary(row, &clusters))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::state::TestState;

    fn login_of(status: ExecJobStatus, ssh: Option<bool>) -> Option<String> {
        let state = TestState::new().cli(|cli| cli.ssh.enable = true).build();
        let job = MExecJob {
            status,
            ..Default::default()
        };
        let spec = ExecSpec {
            ssh,
            ..Default::default()
        };
        ssh_access(&state.config, "infra", &job, &spec).0
    }

    #[test]
    fn the_ssh_login_shows_only_while_the_guest_runs_with_ssh() {
        assert!(login_of(ExecJobStatus::Running, None).is_some());
        assert_eq!(login_of(ExecJobStatus::Running, Some(false)), None);
        for status in [
            ExecJobStatus::Created,
            ExecJobStatus::Queued,
            ExecJobStatus::Starting,
            ExecJobStatus::Completed,
        ] {
            assert_eq!(login_of(status, None), None, "{status:?}");
        }
    }
}
