/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::board_subjects::{
    BoardProject, JobEvaluationView, JobSubjects, WorkerNames, board_projects, job_evaluation,
};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::evals::EvalAccessContext;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::lookup::require;
use crate::metrics_scope::MetricsScope;
use crate::resource::Resource;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::lookup::by_id;
use gradient_db::metrics::build_times::{self, ExpensiveBuild, TopProjectBuildTime};
use gradient_db::metrics::resource_usage::{
    self, ExpensiveEval, ExpensiveResource, ResourceMetric,
};
use gradient_entity::dispatched_job::DispatchedJobKind;
use gradient_entity::{build_attempt, flake_output_node};
use gradient_scheduler::Scheduler;
use gradient_types::events::{Envelope, Event, EventRx, names, worker};
use gradient_types::ids::DispatchedJobId;
use gradient_types::*;
use gradient_util::shutdown::CancellationToken;
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct DispatchedJobSummary {
    pub id: Uuid,
    pub kind: i16,
    pub project: Uuid,
    pub worker_id: String,
    pub worker_name: Option<String>,
    pub score: f64,
    pub dispatched_at: String,
    pub build_id: Option<Uuid>,
    pub evaluation_id: Uuid,
    pub subject: Option<String>,
}

#[derive(Serialize)]
pub struct DispatchedJobsResponse {
    pub jobs: Vec<DispatchedJobSummary>,
    pub other_running: u64,
}

#[derive(Serialize)]
pub struct PendingJobSummary {
    pub kind: i16,
    pub project: Uuid,
    pub evaluation_id: Uuid,
    pub build_id: Option<Uuid>,
    pub queued_at: String,
    pub dependency_count: u32,
    pub subject: Option<String>,
}

#[derive(Serialize)]
pub struct PendingJobsResponse {
    pub jobs: Vec<PendingJobSummary>,
    pub other_pending: u64,
}

pub async fn get_pending_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<PendingJobsResponse>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let (snapshot, hidden): (Vec<_>, Vec<_>) = scheduler
        .pending_jobs_snapshot()
        .await
        .into_iter()
        .partition(|j| scope.allows(&Uuid::from(j.project)));
    let other_pending = hidden.len() as u64;

    let shared_builds: Vec<DerivationBuildId> =
        snapshot.iter().filter_map(|j| j.derivation_build).collect();
    let evaluations: Vec<EvaluationId> = snapshot
        .iter()
        .filter(|j| j.derivation_build.is_none())
        .map(|j| j.evaluation_id)
        .collect();
    let subjects = JobSubjects::load(&state.web_db, &shared_builds, &evaluations).await?;

    let jobs = snapshot
        .into_iter()
        .map(|j| PendingJobSummary {
            kind: i16::from(j.kind),
            project: j.project.into(),
            evaluation_id: j.evaluation_id.into(),
            build_id: j.derivation_build.map(Into::into),
            queued_at: j.queued_at.and_utc().to_rfc3339(),
            dependency_count: j.dependency_count,
            subject: subjects.subject(j.derivation_build, j.evaluation_id),
        })
        .collect();

    Ok(ok_json(PendingJobsResponse {
        jobs,
        other_pending,
    }))
}

pub async fn get_dispatched_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
) -> WebResult<Json<BaseResponse<DispatchedJobsResponse>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let open = gradient_db::worker_registrations::open_dispatched_jobs(&state.web_db, 500).await?;

    let (visible, other_running): (Vec<_>, Vec<_>) = open
        .into_iter()
        .partition(|j| scope.allows(&Uuid::from(j.project)));
    let other_running = other_running.len() as u64;

    // Three reads are made for the whole page instead of three per row. The board is polling this
    // list, and it can carry up to 500 open dispatches.
    let job_ids: Vec<DispatchedJobId> = visible.iter().map(|j| j.id).collect();
    let attempts: HashMap<DispatchedJobId, build_attempt::Model> =
        gradient_db::scheduling::build_attempt::of_dispatched_jobs(&state.web_db, job_ids)
            .await?
            .into_iter()
            .filter_map(|a| Some((a.dispatched_job?, a)))
            .collect();

    let shared_builds: Vec<DerivationBuildId> =
        attempts.values().map(|a| a.derivation_build).collect();
    let evaluations: Vec<EvaluationId> = visible
        .iter()
        .filter(|j| j.kind == DispatchedJobKind::Eval)
        .map(|j| j.evaluation_id)
        .collect();
    let subjects = JobSubjects::load(&state.web_db, &shared_builds, &evaluations).await?;
    let worker_names = WorkerNames::load(
        &state.web_db,
        &scope,
        visible.iter().map(|j| j.worker_id.clone()),
    )
    .await?;

    let mut jobs = Vec::with_capacity(visible.len());
    for j in visible {
        let attempt = attempts.get(&j.id);
        let subject = subjects.subject(attempt.map(|a| a.derivation_build), j.evaluation_id);

        jobs.push(DispatchedJobSummary {
            id: j.id.into(),
            kind: i16::from(j.kind),
            project: j.project.into(),
            worker_name: worker_names.name(&j.worker_id),
            worker_id: j.worker_id,
            score: j.score,
            dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
            build_id: attempt.map(|a| a.derivation_build.into()),
            evaluation_id: j.evaluation_id.into(),
            subject,
        });
    }

    Ok(ok_json(DispatchedJobsResponse {
        jobs,
        other_running,
    }))
}

#[derive(Serialize)]
pub struct DecisionCandidateView {
    pub id: Uuid,
    pub job_id: String,
    pub kind: i16,
    pub project: Uuid,
    pub build_id: Option<Uuid>,
    pub evaluation_id: Uuid,
    pub subject: Option<String>,
    pub score: f64,
    pub won: bool,
}

#[derive(Serialize)]
pub struct AssignDecisionView {
    pub at: String,
    pub worker_id: String,
    pub worker_name: Option<String>,
    pub kind: i16,
    pub winner: Option<String>,
    pub candidates: Vec<DecisionCandidateView>,
}

pub async fn get_assign_decisions(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<AssignDecisionView>>>> {
    require_superuser(&user)?;

    let decisions = scheduler.recent_decisions().await;
    let candidates = decisions.iter().flat_map(|d| &d.candidates);
    let shared_builds: Vec<DerivationBuildId> = candidates
        .clone()
        .filter_map(|c| c.derivation_build)
        .collect();
    let evaluations: Vec<EvaluationId> = candidates
        .filter(|c| c.derivation_build.is_none())
        .map(|c| c.evaluation_id)
        .collect();
    let subjects = JobSubjects::load(&state.web_db, &shared_builds, &evaluations).await?;
    let worker_names = WorkerNames::load(
        &state.web_db,
        &MetricsScope::All,
        decisions.iter().map(|d| d.worker_id.clone()),
    )
    .await?;

    let views = decisions
        .into_iter()
        .map(|d| AssignDecisionView {
            at: d.at.and_utc().to_rfc3339(),
            worker_name: worker_names.name(&d.worker_id),
            worker_id: d.worker_id,
            kind: d.kind,
            winner: d.winner,
            candidates: d
                .candidates
                .into_iter()
                .map(|c| DecisionCandidateView {
                    id: c.id.into(),
                    job_id: c.job_id,
                    kind: c.kind,
                    project: c.project.into(),
                    build_id: c.derivation_build.map(Into::into),
                    evaluation_id: c.evaluation_id.into(),
                    subject: subjects.subject(c.derivation_build, c.evaluation_id),
                    score: c.score,
                    won: c.won,
                })
                .collect(),
        })
        .collect();

    Ok(ok_json(views))
}

#[derive(Serialize)]
pub struct AttemptSummary {
    pub dispatched_job_id: Option<Uuid>,
    pub substitute: bool,
    pub outcome: i32,
    pub reason: Option<i32>,
    pub failure_message: Option<String>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct JobPhaseView {
    pub seq: i32,
    pub parent_seq: Option<i32>,
    pub phase: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub paths: i32,
    pub bytes: i64,
}

async fn job_phases<C: ConnectionTrait>(db: &C, job: DispatchedJobId) -> Vec<JobPhaseView> {
    gradient_db::worker_registrations::phases_of_dispatched_job(db, job)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| JobPhaseView {
            seq: p.seq,
            parent_seq: p.parent_seq,
            phase: gradient_wire::types::JobPhase::name_of(p.phase).into_owned(),
            start_ms: p.start_ms,
            end_ms: p.end_ms,
            paths: p.paths,
            bytes: p.bytes,
        })
        .collect()
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub struct ReportGap {
    pub worker_elapsed_ms: Option<i64>,
    pub worker_tail_ms: Option<i64>,
    pub transit_ms: Option<i64>,
}

fn report_gap(
    worker_elapsed_ms: Option<i64>,
    phases: &[JobPhaseView],
    dispatched_at: NaiveDateTime,
    finished_at: Option<NaiveDateTime>,
) -> ReportGap {
    let Some(elapsed) = worker_elapsed_ms else {
        return ReportGap::default();
    };

    let last_end = phases.iter().map(|p| p.end_ms).max().unwrap_or(0);
    ReportGap {
        worker_elapsed_ms: Some(elapsed),
        worker_tail_ms: Some(elapsed - last_end),
        transit_ms: finished_at.map(|f| (f - dispatched_at).num_milliseconds() - elapsed),
    }
}

#[derive(Serialize)]
pub struct JobDerivationView {
    pub build: Option<Uuid>,
    pub derivation_build: Uuid,
    pub drv_path: String,
    pub pname: Option<String>,
}

fn snapshot_derivations(
    job_context: &serde_json::Value,
) -> Vec<(DerivationBuildId, String, Option<String>)> {
    job_context
        .get("derivations")
        .and_then(|d| d.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|d| {
                    Some((
                        DerivationBuildId::from(d.get("build_id")?.as_str()?.parse::<Uuid>().ok()?),
                        d.get("drv_path")?.as_str()?.to_string(),
                        d.get("pname").and_then(|p| p.as_str()).map(str::to_string),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The scheduler is scoring and dispatching shared builds (`derivation_build`). The API is
/// navigating builds by their per-eval `build_job` id. Every shared build leaving this endpoint is
/// resolved against the job's evaluation first.
async fn resolve_build_jobs<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    shared_builds: &[DerivationBuildId],
) -> HashMap<DerivationBuildId, BuildJobId> {
    if shared_builds.is_empty() {
        return HashMap::new();
    }

    gradient_db::build_jobs::of_derivation_builds_in_evaluation(
        db,
        evaluation,
        shared_builds.to_vec(),
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|j| (j.derivation_build, j.id))
    .collect()
}

async fn job_derivations<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    job_context: &serde_json::Value,
) -> Vec<JobDerivationView> {
    let entries = snapshot_derivations(job_context);
    let shared_builds: Vec<DerivationBuildId> = entries.iter().map(|(a, _, _)| *a).collect();
    let builds = resolve_build_jobs(db, evaluation, &shared_builds).await;

    entries
        .into_iter()
        .map(|(shared_build, drv_path, pname)| JobDerivationView {
            build: builds.get(&shared_build).copied().map(Into::into),
            derivation_build: shared_build.into(),
            drv_path,
            pname,
        })
        .collect()
}

#[derive(Serialize)]
pub struct DispatchedJobDetail {
    pub id: Uuid,
    pub kind: i16,
    pub project: Uuid,
    pub project_name: String,
    pub project_display_name: String,
    pub worker_id: String,
    pub worker_name: Option<String>,
    pub score: f64,
    pub queued_at: String,
    pub dispatched_at: String,
    pub finished_at: Option<String>,
    pub ready_at: Option<String>,
    pub outcome: Option<String>,
    pub phases: Vec<JobPhaseView>,
    #[serde(flatten)]
    pub report_gap: ReportGap,
    pub build_id: Option<Uuid>,
    pub derivation_build_id: Option<Uuid>,
    pub derivations: Vec<JobDerivationView>,
    pub evaluation_id: Uuid,
    pub evaluation: Option<JobEvaluationView>,
    pub pname: Option<String>,
    pub score_breakdown: serde_json::Value,
    pub worker_context: serde_json::Value,
    pub job_context: serde_json::Value,
    pub instance_context: serde_json::Value,
    pub candidates: Option<serde_json::Value>,
    pub previous_attempts: Vec<AttemptSummary>,
    pub passed_over: bool,
}

pub async fn get_dispatched_job(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<DispatchedJobDetail>>> {
    use gradient_entity::dispatched_job::DispatchedJobOutcome;

    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    if let Some(c) = scheduler.candidate_detail(DispatchedJobId::from(id)).await {
        if !scope.allows(&Uuid::from(c.project)) {
            return Err(WebError::not_found(Resource::Job));
        }

        let (project, worker_name) =
            job_names(&state.web_db, &scope, c.project.into(), &c.worker_id).await?;

        let derivations = job_derivations(&state.web_db, c.evaluation_id, &c.job_context).await;
        let build_id = match c.derivation_build {
            Some(shared_build) => {
                resolve_build_jobs(&state.web_db, c.evaluation_id, &[shared_build])
                    .await
                    .get(&shared_build)
                    .copied()
                    .map(Into::into)
            }
            None => None,
        };

        return Ok(ok_json(DispatchedJobDetail {
            id: c.id.into(),
            kind: c.kind,
            project: c.project.into(),
            project_name: project.name,
            project_display_name: project.display_name,
            worker_id: c.worker_id,
            worker_name,
            score: c.score,
            queued_at: c.queued_at.and_utc().to_rfc3339(),
            dispatched_at: c.scored_at.and_utc().to_rfc3339(),
            finished_at: None,
            ready_at: None,
            outcome: None,
            phases: Vec::new(),
            report_gap: ReportGap::default(),
            build_id,
            derivation_build_id: c.derivation_build.map(Into::into),
            derivations,
            evaluation_id: c.evaluation_id.into(),
            evaluation: eval_job_evaluation(&state.web_db, c.kind, c.evaluation_id).await?,
            pname: c.pname,
            score_breakdown: c.score_breakdown,
            worker_context: c.worker_context,
            job_context: c.job_context,
            instance_context: c.instance_context,
            candidates: None,
            previous_attempts: Vec::new(),
            passed_over: !c.won,
        }));
    }

    let j = require::<gradient_entity::dispatched_job::Entity>(
        &state.web_db,
        DispatchedJobId::from(id),
    )
    .await?;

    if !scope.allows(&Uuid::from(j.project)) {
        return Err(WebError::not_found(Resource::Job));
    }

    let (project, worker_name) =
        job_names(&state.web_db, &scope, j.project.into(), &j.worker_id).await?;

    let this_attempt =
        gradient_db::scheduling::build_attempt::of_dispatched_job(&state.web_db, j.id)
            .await
            .ok()
            .flatten();

    let shared_build_id: Option<DerivationBuildId> =
        this_attempt.as_ref().map(|a| a.derivation_build);
    let build_id: Option<Uuid> = match shared_build_id {
        Some(shared_build) => resolve_build_jobs(&state.web_db, j.evaluation_id, &[shared_build])
            .await
            .get(&shared_build)
            .copied()
            .map(Into::into),
        None => None,
    };
    let derivations = job_derivations(&state.web_db, j.evaluation_id, &j.job_context).await;
    let phases = job_phases(&state.web_db, j.id).await;

    let pname = match shared_build_id {
        Some(aid) => {
            let shared_build = by_id::<EDerivationBuild, _>(&state.web_db, aid)
                .await
                .ok()
                .flatten();
            match shared_build {
                Some(shared_build) => by_id::<gradient_entity::derivation::Entity, _>(
                    &state.web_db,
                    shared_build.derivation,
                )
                .await
                .ok()
                .flatten()
                .and_then(|d| d.pname),
                None => None,
            }
        }
        None => None,
    };

    let previous_attempts = match shared_build_id {
        Some(aid) => {
            gradient_db::scheduling::build_attempt::attempts_oldest_first(&state.web_db, aid)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|a| AttemptSummary {
                    dispatched_job_id: a.dispatched_job.map(Into::into),
                    substitute: a.substitute,
                    outcome: i32::from(a.outcome),
                    reason: a.reason.map(i32::from),
                    failure_message: a.failure_message,
                    created_at: a.created_at.and_utc().to_rfc3339(),
                })
                .collect()
        }
        None => Vec::new(),
    };

    Ok(ok_json(DispatchedJobDetail {
        id: j.id.into(),
        kind: i16::from(j.kind),
        project: j.project.into(),
        project_name: project.name,
        project_display_name: project.display_name,
        worker_id: j.worker_id,
        worker_name,
        score: j.score,
        queued_at: j.queued_at.and_utc().to_rfc3339(),
        dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
        finished_at: j.finished_at.map(|t| t.and_utc().to_rfc3339()),
        ready_at: j.ready_at.map(|t| t.and_utc().to_rfc3339()),
        outcome: j.outcome.map(|o| match o {
            DispatchedJobOutcome::Completed => "completed".to_string(),
            DispatchedJobOutcome::Failed => "failed".to_string(),
            DispatchedJobOutcome::Abandoned => "abandoned".to_string(),
        }),
        report_gap: report_gap(j.worker_elapsed_ms, &phases, j.dispatched_at, j.finished_at),
        phases,
        build_id,
        derivation_build_id: shared_build_id.map(Into::into),
        derivations,
        evaluation_id: j.evaluation_id.into(),
        evaluation: eval_job_evaluation(&state.web_db, i16::from(j.kind), j.evaluation_id).await?,
        pname,
        score_breakdown: j.score_breakdown,
        worker_context: j.worker_context,
        job_context: j.job_context,
        instance_context: j.instance_context.unwrap_or(serde_json::Value::Null),
        candidates: if scope.is_all() { j.candidates } else { None },
        previous_attempts,
        passed_over: false,
    }))
}

async fn job_names<C: ConnectionTrait>(
    db: &C,
    scope: &MetricsScope,
    project: Uuid,
    worker: &str,
) -> Result<(BoardProject, Option<String>), sea_orm::DbErr> {
    let project = board_projects(db, &[project])
        .await?
        .remove(&project)
        .unwrap_or_default();
    let worker_name = WorkerNames::load(db, scope, [worker.to_string()])
        .await?
        .name(worker);

    Ok((project, worker_name))
}

async fn eval_job_evaluation<C: ConnectionTrait>(
    db: &C,
    kind: i16,
    evaluation: EvaluationId,
) -> Result<Option<JobEvaluationView>, sea_orm::DbErr> {
    if kind != i16::from(DispatchedJobKind::Eval) {
        return Ok(None);
    }

    job_evaluation(db, evaluation).await
}

#[derive(Serialize)]
pub struct BoardWorker {
    pub id: Option<String>,
    pub name: Option<String>,
    pub projects: Vec<BoardProject>,
    pub draining: bool,
    pub assigned_jobs: i64,
    pub max_concurrent_builds: i64,
    pub eval: bool,
    pub fetch: bool,
    pub build: bool,
    pub architectures: Vec<String>,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<i64>,
    pub ram_total_mb: Option<i64>,
}

pub async fn get_board_workers(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<BoardWorker>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let workers: Vec<_> = scheduler
        .board_workers()
        .await
        .into_iter()
        .map(|w| (scope.worker_projects(w.authorized_peers.as_ref()), w))
        .collect();

    let visible_projects: Vec<Uuid> = workers
        .iter()
        .flat_map(|(projects, _)| projects.iter().flatten().copied())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let projects_by_id = board_projects(&state.web_db, &visible_projects).await?;
    let worker_names = WorkerNames::load(
        &state.web_db,
        &scope,
        workers
            .iter()
            .filter(|(projects, _)| projects.is_some())
            .map(|(_, w)| w.id.clone()),
    )
    .await?;

    let out = workers
        .into_iter()
        .map(|(projects, w)| {
            let accessible = projects.is_some();

            BoardWorker {
                id: accessible.then(|| w.id.clone()),
                name: accessible.then(|| worker_names.name(&w.id)).flatten(),
                projects: projects
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|p| projects_by_id.get(p).cloned())
                    .collect(),
                draining: w.draining,
                assigned_jobs: w.assigned_job_count as i64,
                max_concurrent_builds: w.max_concurrent_builds as i64,
                eval: w.capabilities.eval,
                fetch: w.capabilities.fetch,
                build: w.capabilities.build,
                architectures: if accessible { w.architectures } else { vec![] },
                cpu_usage_pct: accessible.then_some(w.cpu_usage_pct).flatten(),
                ram_free_mb: accessible
                    .then_some(w.ram_free_mb.map(|v| v as i64))
                    .flatten(),
                ram_total_mb: accessible.then_some(w.ram_total_mb as i64),
            }
        })
        .collect();

    Ok(ok_json(out))
}

#[derive(Serialize, Debug, PartialEq)]
pub struct LoadBucket {
    pub key: String,
    pub in_flight: u32,
    pub capacity: u32,
    pub workers: u32,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct WorkerLoad {
    pub by_capability: Vec<LoadBucket>,
    pub by_architecture: Vec<LoadBucket>,
    pub by_feature: Vec<LoadBucket>,
}

type LoadAcc = std::collections::HashMap<String, (u32, u32, u32)>;

fn bump_capacity(acc: &mut LoadAcc, key: &str, slots: u32) {
    let e = acc.entry(key.to_owned()).or_default();
    e.1 += slots;
    e.2 += 1;
}

fn bump_in_flight(acc: &mut LoadAcc, key: &str) {
    acc.entry(key.to_owned()).or_default().0 += 1;
}

fn buckets_sorted(acc: LoadAcc) -> Vec<LoadBucket> {
    let mut out: Vec<LoadBucket> = acc
        .into_iter()
        .map(|(key, (in_flight, capacity, workers))| LoadBucket {
            key,
            in_flight,
            capacity,
            workers,
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

fn aggregate_worker_load(
    workers: &[&gradient_pool::WorkerInfo],
    jobs: &[&gradient_scheduler::BoardActiveJob],
) -> WorkerLoad {
    use gradient_entity::dispatched_job::DispatchedJobKind;

    let mut cap: LoadAcc = LoadAcc::new();
    let mut arch: LoadAcc = LoadAcc::new();
    let mut feat: LoadAcc = LoadAcc::new();

    for w in workers {
        let slots = w.max_concurrent_builds;
        if w.capabilities.eval {
            bump_capacity(&mut cap, "eval", slots);
        }
        if w.capabilities.fetch {
            bump_capacity(&mut cap, "fetch", slots);
        }
        if w.capabilities.build {
            bump_capacity(&mut cap, "build", slots);
        }
        for a in &w.architectures {
            bump_capacity(&mut arch, a, slots);
        }
        for f in &w.system_features {
            bump_capacity(&mut feat, f, slots);
        }
    }

    for j in jobs {
        match j.kind {
            DispatchedJobKind::Build => bump_in_flight(&mut cap, "build"),
            DispatchedJobKind::Eval => {
                if j.eval_step {
                    bump_in_flight(&mut cap, "eval");
                }
                if j.fetch_step {
                    bump_in_flight(&mut cap, "fetch");
                }
            }
        }
        if let Some(a) = &j.architecture
            && a != BUILTIN_ARCH
        {
            bump_in_flight(&mut arch, a);
        }
        for f in &j.required_features {
            bump_in_flight(&mut feat, f);
        }
    }

    let by_capability = ["eval", "fetch", "build"]
        .into_iter()
        .map(|k| {
            let (in_flight, capacity, workers) = cap.get(k).copied().unwrap_or_default();
            LoadBucket {
                key: k.to_owned(),
                in_flight,
                capacity,
                workers,
            }
        })
        .collect();

    WorkerLoad {
        by_capability,
        by_architecture: buckets_sorted(arch),
        by_feature: buckets_sorted(feat),
    }
}

pub async fn get_board_worker_load(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<WorkerLoad>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let workers = scheduler.board_workers().await;
    let jobs = scheduler.board_active_jobs().await;

    let visible_workers: Vec<&gradient_pool::WorkerInfo> = workers
        .iter()
        .filter(|w| scope.worker_projects(w.authorized_peers.as_ref()).is_some())
        .collect();
    let visible_jobs: Vec<&gradient_scheduler::BoardActiveJob> = jobs
        .iter()
        .filter(|j| scope.allows(&Uuid::from(j.project)))
        .collect();

    Ok(ok_json(aggregate_worker_load(
        &visible_workers,
        &visible_jobs,
    )))
}

#[derive(Deserialize)]
pub struct ExpensiveParams {
    pub window_days: Option<i64>,
}

pub async fn get_expensive_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<ExpensiveParams>,
) -> WebResult<Json<BaseResponse<Vec<ExpensiveBuild>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    let project_filter = scope.project_in_list();
    if let Some(list) = &project_filter
        && list.is_empty()
    {
        return Ok(ok_json(vec![]));
    }

    let window = params.window_days.unwrap_or(30).max(1);

    let out =
        build_times::expensive_builds(&state.web_db, window, project_filter.as_deref()).await?;

    Ok(ok_json(out))
}

#[derive(Deserialize)]
pub struct ScoringParams {
    pub window_hours: Option<i64>,
    pub limit: Option<u64>,
}

#[derive(Serialize)]
pub struct ScoreBucket {
    pub lo: f64,
    pub hi: f64,
    pub count: i64,
}

#[derive(Serialize)]
pub struct RuleContribution {
    pub rule: String,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Serialize, Default)]
pub struct ScoringSummary {
    pub sample_size: i64,
    pub score_min: f64,
    pub score_max: f64,
    pub score_avg: f64,
    pub histogram: Vec<ScoreBucket>,
    pub rules: Vec<RuleContribution>,
}

#[derive(Serialize)]
pub struct RuleDescription {
    pub rule: String,
    pub description: String,
}

pub async fn get_scoring_rules() -> WebResult<Json<BaseResponse<Vec<RuleDescription>>>> {
    let rules = gradient_pool::score::rule_catalog()
        .into_iter()
        .map(|(rule, description)| RuleDescription {
            rule: rule.to_string(),
            description: description.to_string(),
        })
        .collect();

    Ok(ok_json(rules))
}

pub async fn get_scoring_summary(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<ScoringParams>,
) -> WebResult<Json<BaseResponse<ScoringSummary>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = params.window_hours.unwrap_or(24).max(1);
    let limit = params.limit.unwrap_or(2000).min(10_000);

    let project_filter = scope.project_in_list();
    if let Some(list) = &project_filter
        && list.is_empty()
    {
        return Ok(ok_json(ScoringSummary::default()));
    }

    let rows = gradient_db::worker_registrations::recent_scores(
        &state.web_db,
        window,
        limit,
        project_filter.as_deref(),
    )
    .await?;

    let mut scores: Vec<f64> = Vec::with_capacity(rows.len());
    let mut rule_acc: std::collections::BTreeMap<String, (f64, i64, f64, f64)> =
        std::collections::BTreeMap::new();

    for r in &rows {
        scores.push(r.score);
        if let Some(bd) = &r.score_breakdown
            && let Some(obj) = bd.get("rules").and_then(|v| v.as_object())
        {
            for (rule, val) in obj {
                if let Some(v) = val.as_f64() {
                    let e = rule_acc
                        .entry(rule.clone())
                        .or_insert((0.0, 0, f64::MAX, f64::MIN));
                    e.0 += v;
                    e.1 += 1;
                    e.2 = e.2.min(v);
                    e.3 = e.3.max(v);
                }
            }
        }
    }

    if scores.is_empty() {
        return Ok(ok_json(ScoringSummary::default()));
    }

    let n = scores.len() as f64;
    let lo = scores.iter().cloned().fold(f64::MAX, f64::min);
    let hi = scores.iter().cloned().fold(f64::MIN, f64::max);
    let avg = scores.iter().sum::<f64>() / n;

    const BINS: usize = 12;
    let span = (hi - lo).max(f64::EPSILON);
    let mut counts = vec![0i64; BINS];
    for s in &scores {
        let idx = (((s - lo) / span) * BINS as f64).floor() as usize;
        counts[idx.min(BINS - 1)] += 1;
    }

    let histogram = counts
        .into_iter()
        .enumerate()
        .map(|(i, count)| ScoreBucket {
            lo: lo + span * (i as f64) / BINS as f64,
            hi: lo + span * (i as f64 + 1.0) / BINS as f64,
            count,
        })
        .collect();

    let mut rules: Vec<RuleContribution> = rule_acc
        .into_iter()
        .map(|(rule, (sum, count, min, max))| RuleContribution {
            rule,
            avg: if count > 0 { sum / count as f64 } else { 0.0 },
            min,
            max,
        })
        .collect();

    rules.sort_by(|a, b| {
        b.avg
            .abs()
            .partial_cmp(&a.avg.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(ok_json(ScoringSummary {
        sample_size: scores.len() as i64,
        score_min: lo,
        score_max: hi,
        score_avg: avg,
        histogram,
        rules,
    }))
}

pub async fn get_top_projects_by_buildtime(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<ExpensiveParams>,
) -> WebResult<Json<BaseResponse<Vec<TopProjectBuildTime>>>> {
    require_superuser(&user)?;
    let window = params.window_days.unwrap_or(30).max(1);

    let out = build_times::top_projects(&state.web_db, window).await?;

    Ok(ok_json(out))
}

#[derive(Deserialize)]
pub struct ResourceParams {
    pub metric: String,
    pub window_days: Option<i64>,
}

pub async fn get_expensive_by_resource(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<ResourceParams>,
) -> WebResult<Json<BaseResponse<Vec<ExpensiveResource>>>> {
    let metric = ResourceMetric::of_build(&params.metric)
        .ok_or_else(|| WebError::not_found(Resource::Metric))?;
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    let project_filter = scope.project_in_list();
    if let Some(list) = &project_filter
        && list.is_empty()
    {
        return Ok(ok_json(vec![]));
    }

    let window = params.window_days.unwrap_or(30).max(1);

    let out = resource_usage::expensive_derivations(
        &state.web_db,
        metric,
        window,
        project_filter.as_deref(),
    )
    .await?;

    Ok(ok_json(out))
}

pub async fn board_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    ws: WebSocketUpgrade,
) -> Response {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user)
        .await
        .unwrap_or(MetricsScope::Projects(vec![]));

    let rx = state.events.subscribe();
    let (workers, pending, active) = scheduler.metrics_snapshot().await;
    let initial = Envelope::now(
        worker::QueueDepth {
            workers,
            pending,
            active,
        }
        .into(),
    );
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(board_live_loop(socket, rx, scope, initial, cancel))
            .await;
    })
}

async fn board_live_loop(
    mut socket: WebSocket,
    rx: EventRx,
    scope: MetricsScope,
    initial: Envelope,
    cancel: CancellationToken,
) {
    if let Some(text) = mask_event(&initial, &scope)
        && socket.send(Message::Text(text.into())).await.is_err()
    {
        return;
    }

    super::live::live_stream(
        socket,
        rx,
        move |env| mask_event(env, &scope),
        super::live::skip_lag,
        cancel,
    )
    .await;
}

fn mask_event(env: &Envelope, scope: &MetricsScope) -> Option<String> {
    match &env.event {
        Event::WorkerQueueDepth(_) => Some(env.to_line()),
        Event::WorkerJobDispatched(j) if scope.allows(&j.project.into_inner()) => {
            Some(env.to_line())
        }
        Event::WorkerConnected(w) if w.projects.iter().any(|p| scope.allows(&p.into_inner())) => {
            Some(
                serde_json::json!({
                    "event": names::WORKER_CONNECTED,
                    "at": env.at,
                    "content": { "worker_id": w.worker_id },
                })
                .to_string(),
            )
        }
        Event::WorkerDisconnected(_) if scope.is_all() => Some(env.to_line()),
        _ => None,
    }
}

#[derive(Deserialize)]
pub struct EvalResourceParams {
    pub metric: String,
    pub window_days: Option<i64>,
}

pub async fn get_expensive_evals_by_resource(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<EvalResourceParams>,
) -> WebResult<Json<BaseResponse<Vec<ExpensiveEval>>>> {
    let metric = ResourceMetric::of_evaluation(&params.metric)
        .ok_or_else(|| WebError::not_found(Resource::Metric))?;
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    let project_filter = scope.project_in_list();
    if let Some(list) = &project_filter
        && list.is_empty()
    {
        return Ok(ok_json(vec![]));
    }

    let window = params.window_days.unwrap_or(30).max(1);

    let out = resource_usage::expensive_evaluations(
        &state.web_db,
        metric,
        window,
        project_filter.as_deref(),
    )
    .await?;

    Ok(ok_json(out))
}

#[derive(Serialize)]
pub struct FlakeGraphNode {
    pub path: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_derivation: bool,
    pub drv_path: Option<String>,
}

impl From<flake_output_node::Model> for FlakeGraphNode {
    fn from(n: flake_output_node::Model) -> Self {
        Self {
            path: n.path,
            parent: n.parent,
            name: n.name,
            kind: n.kind,
            is_derivation: n.is_derivation,
            drv_path: n.drv_path.map(|p| {
                gradient_entity::StorePath::parse(&p)
                    .map(|sp| sp.base())
                    .unwrap_or(p)
            }),
        }
    }
}

pub async fn get_eval_flake_graph(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<Vec<FlakeGraphNode>>>> {
    let _ctx = EvalAccessContext::load(&state.web_db, evaluation_id, &maybe_user, api_key.as_ref())
        .await?;
    let rows = gradient_db::flake_output_nodes::of_evaluation(&state.web_db, evaluation_id).await?;

    let out = rows.into_iter().map(FlakeGraphNode::from).collect();
    Ok(ok_json(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::dispatched_job::DispatchedJobKind;
    use gradient_pool::WorkerInfo;
    use gradient_scheduler::BoardActiveJob;
    use gradient_types::ids::ProjectId;
    use gradient_wire::types::GradientCapabilities;

    fn worker(
        eval: bool,
        fetch: bool,
        build: bool,
        arch: &[&str],
        features: &[&str],
        slots: u32,
    ) -> WorkerInfo {
        WorkerInfo {
            id: "w".into(),
            capabilities: GradientCapabilities {
                eval,
                fetch,
                build,
                ..Default::default()
            },
            architectures: arch.iter().map(|s| s.to_string()).collect(),
            system_features: features.iter().map(|s| s.to_string()).collect(),
            max_concurrent_builds: slots,
            assigned_job_count: 0,
            assigned_build_count: 0,
            draining: false,
            authorized_peers: None,
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            upload_speed_mbps: None,
            download_speed_mbps: None,
        }
    }

    #[test]
    fn a_worker_connection_reaches_each_served_project_without_naming_the_others() {
        let mine = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        let ev = Envelope::now(
            worker::Connected {
                projects: vec![ProjectId::new(other), ProjectId::new(mine)],
                worker_id: "w".into(),
            }
            .into(),
        );

        let text = mask_event(&ev, &MetricsScope::Projects(vec![mine.to_string()]))
            .expect("a member of a served project sees the connection");
        assert!(!text.contains(&other.to_string()), "{text}");
        assert!(
            mask_event(
                &ev,
                &MetricsScope::Projects(vec![uuid::Uuid::now_v7().to_string()])
            )
            .is_none()
        );
    }

    fn phase(seq: i32, start_ms: i64, end_ms: i64) -> JobPhaseView {
        JobPhaseView {
            seq,
            parent_seq: None,
            phase: "build".into(),
            start_ms,
            end_ms,
            paths: 0,
            bytes: 0,
        }
    }

    #[test]
    fn the_report_gap_splits_into_worker_tail_and_transit() {
        let dispatched_at = now();
        let finished_at = dispatched_at + chrono::Duration::milliseconds(10_000);
        let phases = [phase(0, 0, 6_000), phase(1, 100, 8_500)];

        let gap = report_gap(Some(9_000), &phases, dispatched_at, Some(finished_at));

        assert_eq!(gap.worker_elapsed_ms, Some(9_000));
        assert_eq!(gap.worker_tail_ms, Some(500));
        assert_eq!(gap.transit_ms, Some(1_000));
    }

    #[test]
    fn a_row_without_a_worker_clock_has_no_gap() {
        let dispatched_at = now();

        let gap = report_gap(None, &[phase(0, 0, 10)], dispatched_at, Some(dispatched_at));

        assert_eq!(gap, ReportGap::default());
    }

    fn build_job(arch: &str, features: &[&str]) -> BoardActiveJob {
        BoardActiveJob {
            worker_id: "w".into(),
            project: ProjectId::now_v7(),
            kind: DispatchedJobKind::Build,
            architecture: Some(arch.into()),
            required_features: features.iter().map(|s| s.to_string()).collect(),
            fetch_step: false,
            eval_step: false,
        }
    }

    fn flake_job(eval_step: bool, fetch_step: bool) -> BoardActiveJob {
        BoardActiveJob {
            worker_id: "w".into(),
            project: ProjectId::now_v7(),
            kind: DispatchedJobKind::Eval,
            architecture: None,
            required_features: vec![],
            fetch_step,
            eval_step,
        }
    }

    fn bucket<'a>(load: &'a [LoadBucket], key: &str) -> &'a LoadBucket {
        load.iter().find(|b| b.key == key).expect("bucket present")
    }

    #[test]
    fn worker_load_diverges_per_capability_and_architecture() {
        let all_round = worker(true, true, true, &["x86_64-linux"], &["kvm"], 8);
        let build_only = worker(false, false, true, &["aarch64-linux"], &[], 4);
        let workers: Vec<&WorkerInfo> = vec![&all_round, &build_only];

        let mut jobs = Vec::new();
        for i in 0..6 {
            jobs.push(build_job(
                "x86_64-linux",
                if i < 3 { &["kvm"] } else { &[] },
            ));
        }
        jobs.push(build_job("aarch64-linux", &[]));
        jobs.push(build_job("aarch64-linux", &[]));
        jobs.push(flake_job(true, false));
        jobs.push(flake_job(false, true));
        let job_refs: Vec<&BoardActiveJob> = jobs.iter().collect();

        let load = aggregate_worker_load(&workers, &job_refs);

        let cap = &load.by_capability;
        assert_eq!(
            cap.iter().map(|b| b.key.as_str()).collect::<Vec<_>>(),
            ["eval", "fetch", "build"]
        );
        assert_eq!(
            *bucket(cap, "eval"),
            LoadBucket {
                key: "eval".into(),
                in_flight: 1,
                capacity: 8,
                workers: 1
            }
        );
        assert_eq!(
            *bucket(cap, "fetch"),
            LoadBucket {
                key: "fetch".into(),
                in_flight: 1,
                capacity: 8,
                workers: 1
            }
        );
        assert_eq!(
            *bucket(cap, "build"),
            LoadBucket {
                key: "build".into(),
                in_flight: 8,
                capacity: 12,
                workers: 2
            }
        );

        let arch = &load.by_architecture;
        assert_eq!(
            *bucket(arch, "x86_64-linux"),
            LoadBucket {
                key: "x86_64-linux".into(),
                in_flight: 6,
                capacity: 8,
                workers: 1
            }
        );
        assert_eq!(
            *bucket(arch, "aarch64-linux"),
            LoadBucket {
                key: "aarch64-linux".into(),
                in_flight: 2,
                capacity: 4,
                workers: 1
            }
        );

        let feat = &load.by_feature;
        assert_eq!(
            *bucket(feat, "kvm"),
            LoadBucket {
                key: "kvm".into(),
                in_flight: 3,
                capacity: 8,
                workers: 1
            }
        );
    }

    #[test]
    fn worker_load_ignores_builtin_arch_and_shows_empty_capacity() {
        let w = worker(true, false, true, &["x86_64-linux"], &[], 2);
        let workers: Vec<&WorkerInfo> = vec![&w];
        let jobs = [build_job(BUILTIN_ARCH, &[])];
        let job_refs: Vec<&BoardActiveJob> = jobs.iter().collect();

        let load = aggregate_worker_load(&workers, &job_refs);
        assert!(load.by_architecture.iter().all(|b| b.key != BUILTIN_ARCH));
        assert_eq!(
            *bucket(&load.by_capability, "fetch"),
            LoadBucket {
                key: "fetch".into(),
                in_flight: 0,
                capacity: 0,
                workers: 0
            }
        );
    }

    #[test]
    fn snapshot_derivations_reads_the_scheduler_shared_build_ids() {
        let shared_build = DerivationBuildId::now_v7();
        let ctx = serde_json::json!({
            "derivations": [
                { "build_id": shared_build.to_string(), "drv_path": "aaa-curl.drv", "pname": "curl" },
                { "build_id": "not-a-uuid", "drv_path": "bbb-nope.drv", "pname": null },
            ]
        });

        assert_eq!(
            snapshot_derivations(&ctx),
            vec![(
                shared_build,
                "aaa-curl.drv".to_string(),
                Some("curl".to_string())
            )]
        );
    }

    #[test]
    fn snapshot_derivations_of_an_eval_job_is_empty() {
        assert!(snapshot_derivations(&serde_json::json!({ "kind": "Eval" })).is_empty());
    }

    #[test]
    fn flake_output_node_maps_to_graph_node() {
        let model = flake_output_node::Model {
            id: FlakeOutputNodeId::now_v7(),
            evaluation: EvaluationId::now_v7(),
            path: "packages.x86_64-linux.hello".into(),
            parent: Some("packages.x86_64-linux".into()),
            name: "hello".into(),
            kind: "derivation".into(),
            is_derivation: true,
            drv_path: Some("/nix/store/abc-hello.drv".into()),
        };
        let node = FlakeGraphNode::from(model);
        assert_eq!(node.path, "packages.x86_64-linux.hello");
        assert_eq!(node.parent.as_deref(), Some("packages.x86_64-linux"));
        assert_eq!(node.name, "hello");
        assert_eq!(node.kind, "derivation");
        assert!(node.is_derivation);
        assert_eq!(node.drv_path.as_deref(), Some("abc-hello.drv"));
    }
}
