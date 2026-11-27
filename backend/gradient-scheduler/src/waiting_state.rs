/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::by_id;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use gradient_db::status::update_evaluation_status;
use gradient_db::{DbContext, WorkerDb, database_error_kind_text_and_sqlstate_for_warning_log};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_graph::Graph;
use gradient_pool::{ProjectAccess, WorkerInfo};
use gradient_types::events::worker_demand::WorkerDemand;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait, Value,
};
use tracing::{debug, error, info, warn};

const DRV_RECOVERY_GRACE_SECS: i64 = 120;

const WAITING_REFRESH_DATABASE_POOL_CONNECTION_COUNT: u32 = 2;
const WAITING_REFRESH_SESSION_SETTINGS: [(&str, &str); 2] =
    [("statement_timeout", "5s"), ("lock_timeout", "2s")];

pub(crate) fn waiting_refresh_database_pool(db: &DbContext) -> DbContext {
    DbContext {
        worker_db: db
            .worker_db
            .new_with_separate_connection_pool_and_session_settings(
                WAITING_REFRESH_DATABASE_POOL_CONNECTION_COUNT,
                &WAITING_REFRESH_SESSION_SETTINGS,
            ),
        ..db.clone()
    }
}

pub(crate) struct WaitingRefreshStateBetweenPasses {
    pub(crate) budget: Duration,
    last_refreshed_evaluation_id: Option<EvaluationId>,
    warned_errors_by_evaluation: WaitingRefreshWarningRecord,
    previous_refresh_pass_hit_budget: bool,
}

impl Default for WaitingRefreshStateBetweenPasses {
    fn default() -> Self {
        Self {
            budget: crate::loops::WAITING_REFRESH_TIME_BUDGET,
            last_refreshed_evaluation_id: None,
            warned_errors_by_evaluation: WaitingRefreshWarningRecord::default(),
            previous_refresh_pass_hit_budget: false,
        }
    }
}

impl WaitingRefreshStateBetweenPasses {
    fn log_waiting_refresh_budget_stop(&mut self, refreshed: usize, skipped: usize) {
        const WAITING_REFRESH_PASS_TIME_BUDGET_REACHED_LOG_MESSAGE: &str =
            "waiting refresh exhausted time budget. skipped evaluations come first on next pass";
        if self.previous_refresh_pass_hit_budget {
            debug!(
                refreshed,
                skipped, "{WAITING_REFRESH_PASS_TIME_BUDGET_REACHED_LOG_MESSAGE}"
            );
        } else {
            info!(
                refreshed,
                skipped, "{WAITING_REFRESH_PASS_TIME_BUDGET_REACHED_LOG_MESSAGE}"
            );
        }
        self.previous_refresh_pass_hit_budget = true;
    }

    fn record_waiting_refresh_pass_within_budget(&mut self) {
        self.previous_refresh_pass_hit_budget = false;
    }
}

fn rotate_evaluations_after_last_refreshed<T>(
    sorted_by_id: &mut [T],
    last_refreshed: Option<EvaluationId>,
    id_of: impl Fn(&T) -> EvaluationId,
) {
    let Some(last_refreshed) = last_refreshed else {
        return;
    };
    let start = sorted_by_id.partition_point(|item| id_of(item) <= last_refreshed);
    sorted_by_id.rotate_left(start);
}

#[derive(Default)]
struct WaitingRefreshWarningRecord {
    last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id:
        HashMap<EvaluationId, (&'static str, Option<String>)>,
}

impl WaitingRefreshWarningRecord {
    fn log_waiting_refresh_failure(&mut self, evaluation_id: EvaluationId, err: &anyhow::Error) {
        let reduced_error = database_error_kind_text_and_sqlstate_for_warning_log(err);
        let (error_kind, sqlstate) = (
            reduced_error.0,
            reduced_error.1.as_deref().unwrap_or("none"),
        );
        if self
            .last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id
            .get(&evaluation_id)
            == Some(&reduced_error)
        {
            debug!(
                %evaluation_id,
                error_kind,
                sqlstate,
                error = format!("{err:#}"),
                "update of the waiting state for an evaluation failed again with the same error"
            );
            return;
        }
        warn!(
            %evaluation_id,
            error_kind,
            sqlstate,
            "failed to update waiting state of one evaluation. proceed to next evaluation"
        );
        debug!(
            %evaluation_id,
            error = format!("{err:#}"),
            "failed to update the waiting state of an evaluation"
        );
        self.last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id
            .insert(evaluation_id, reduced_error);
    }

    fn clear_warned_error(&mut self, evaluation_id: EvaluationId) {
        self.last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id
            .remove(&evaluation_id);
    }

    fn retain(&mut self, in_flight: &HashSet<EvaluationId>) {
        self.last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id
            .retain(|evaluation_id, _| in_flight.contains(evaluation_id));
    }
}

use crate::assessment_memo::AssessmentMemo;
use crate::buildability::{BuildWorkerCaps, BuildabilityChecker};
use crate::unbuildable::{Unbuildable, unbuildable};
use gradient_db::evaluations::counters::EvalCounters;

pub(crate) struct WaitingRefresh<'a> {
    pub(crate) db: &'a DbContext,
    pub(crate) graph: &'a Graph,
    pub(crate) memo: &'a Mutex<AssessmentMemo>,
    pub(crate) progress: &'a gradient_util::sync::Mutex<WaitingRefreshStateBetweenPasses>,
}

impl WaitingRefresh<'_> {
    pub(crate) async fn run(
        &self,
        workers: &[WorkerInfo],
        draining: bool,
    ) -> Result<Vec<Unbuildable>> {
        let (budget, last_refreshed_evaluation_id) = {
            let progress = self.progress.lock();
            (progress.budget, progress.last_refreshed_evaluation_id)
        };
        let deadline = Instant::now() + budget;
        gradient_db::evaluations::counters::fold_shared_build_deltas(&self.db.worker_db)
            .await
            .context("fold evaluation shared build deltas")?;

        let evals = EEvaluation::find()
            .filter(CEvaluation::Status.is_in(vec![
                EvaluationStatus::Queued,
                EvaluationStatus::Fetching,
                EvaluationStatus::EvaluatingFlake,
                EvaluationStatus::EvaluatingDerivation,
                EvaluationStatus::Building,
                EvaluationStatus::Waiting,
            ]))
            .order_by_asc(CEvaluation::Id)
            .all(&self.db.worker_db)
            .await
            .context("fetch in-flight evaluations")?;
        if evals.is_empty() {
            return Ok(Vec::new());
        }

        if draining {
            for eval in evals {
                let reason = eval
                    .waiting_reason
                    .as_ref()
                    .and_then(WaitingReason::from_json);
                if eval.status == EvaluationStatus::Waiting
                    && matches!(reason, Some(WaitingReason::Draining))
                {
                    continue;
                }

                let needs_status_change = eval.status != EvaluationStatus::Waiting;
                persist_waiting_reason(
                    &self.db.worker_db,
                    eval.id,
                    &eval.waiting_reason,
                    Some(&WaitingReason::Draining),
                )
                .await;

                if needs_status_change {
                    info!(evaluation_id = %eval.id, from = ?eval.status, "parking evaluation: instance draining");
                    update_evaluation_status(self.db, eval, EvaluationStatus::Waiting).await?;
                }
            }

            return Ok(Vec::new());
        }

        let tasks = EvalTasks::load(&self.db.worker_db, &evals).await?;
        let ids: Vec<EvaluationId> = evals.iter().map(|e| e.id).collect();
        let counters =
            gradient_db::evaluations::counters::in_flight_counters(&self.db.worker_db, &ids)
                .await
                .context("read evaluation shared build counters")?;
        let in_flight = ids.iter().copied().collect::<HashSet<_>>();
        lock(self.memo).retain(&in_flight);
        self.progress
            .lock()
            .warned_errors_by_evaluation
            .retain(&in_flight);
        let mut pass = WaitingRefreshPassSharedInputs {
            deadline,
            workers,
            tasks,
            counters,
            now: gradient_types::now(),
            caps_by_project: HashMap::new(),
            has_writable_cache_by_project: HashMap::new(),
        };

        let mut evals = evals;
        rotate_evaluations_after_last_refreshed(&mut evals, last_refreshed_evaluation_id, |e| e.id);
        let mut unbuildables = Vec::new();
        let in_flight_count = evals.len();
        let mut waiting_refresh_reached_deadline = false;
        for (refreshed, eval) in evals.into_iter().enumerate() {
            if Instant::now() >= pass.deadline {
                self.progress
                    .lock()
                    .log_waiting_refresh_budget_stop(refreshed, in_flight_count - refreshed);
                waiting_refresh_reached_deadline = true;
                break;
            }
            let evaluation_id = eval.id;
            let outcome = self.refresh_evaluation_waiting_state(&mut pass, eval).await;
            let mut progress = self.progress.lock();
            progress.last_refreshed_evaluation_id = Some(evaluation_id);
            match outcome {
                Ok(found) => {
                    progress
                        .warned_errors_by_evaluation
                        .clear_warned_error(evaluation_id);
                    unbuildables.extend(found);
                }
                Err(e) => progress
                    .warned_errors_by_evaluation
                    .log_waiting_refresh_failure(evaluation_id, &e),
            }
        }
        if !waiting_refresh_reached_deadline {
            self.progress
                .lock()
                .record_waiting_refresh_pass_within_budget();
        }

        Ok(unbuildables)
    }

    async fn refresh_evaluation_waiting_state(
        &self,
        pass: &mut WaitingRefreshPassSharedInputs<'_>,
        eval: MEvaluation,
    ) -> Result<Option<Unbuildable>> {
        let workers = pass.workers;
        let deadline = pass.deadline;
        let eval_counters = pass.counters.get(&eval.id).copied().unwrap_or_default();
        let reason = eval
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json);
        let project = pass.tasks.project_of(&eval);
        let project_workers = ProjectWorkerCounts::of(workers, project);
        let worker_caps = pass
            .caps_by_project
            .entry(project)
            .or_insert_with(|| project_worker_caps(workers, project))
            .as_slice();

        if eval.status == EvaluationStatus::Waiting
            && matches!(reason, Some(WaitingReason::CacheStorageFull))
        {
            return Ok(None);
        }

        let cache_outcome = self
            .refresh_waiting_evaluation_cache_state(
                &eval,
                reason.as_ref(),
                project,
                project_workers,
                &mut pass.has_writable_cache_by_project,
            )
            .await?;
        let outcome = if let Some(cache_outcome) = cache_outcome {
            Some(cache_outcome)
        } else {
            match eval.status {
                EvaluationStatus::Waiting => match reason {
                    Some(WaitingReason::EvalWorkers { capability, .. }) => {
                        Some(decide_eval_recovery(capability, project_workers))
                    }
                    Some(WaitingReason::Draining) => Some((eval.status_after_draining(), None)),
                    _ => match self
                        .build_phase_decision(
                            eval.id,
                            eval_counters,
                            worker_caps,
                            reason.as_ref(),
                            deadline,
                        )
                        .await?
                    {
                        BuildPhase::Pending(a) => Some((a.target, a.reason)),
                        BuildPhase::Settled => return Ok(None),
                        BuildPhase::Unnamed => {
                            Some(decide_eval_recovery(EvalCapability::Eval, project_workers))
                        }
                    },
                },
                EvaluationStatus::Queued
                | EvaluationStatus::Fetching
                | EvaluationStatus::EvaluatingFlake
                | EvaluationStatus::EvaluatingDerivation => decide_pre_build_target(
                    eval.kind,
                    eval.status,
                    check_repository_url_is_ssh(&eval.repository),
                    project_workers,
                ),
                EvaluationStatus::Building => {
                    match self
                        .build_phase_decision(
                            eval.id,
                            eval_counters,
                            worker_caps,
                            reason.as_ref(),
                            deadline,
                        )
                        .await?
                    {
                        BuildPhase::Pending(a) => Some((a.target, a.reason)),
                        BuildPhase::Settled | BuildPhase::Unnamed => {
                            self.exec_jobs_wait(eval.id, reason.as_ref()).await?
                        }
                    }
                }
                _ => None,
            }
        };

        let Some((target, new_reason)) = outcome else {
            return Ok(None);
        };

        let waits = pass.tasks.wait_for_workers(&eval);
        if let Some(unmet) =
            unbuildable(&eval, reason.as_ref(), new_reason.as_ref(), waits, pass.now)
        {
            return Ok(Some(Unbuildable {
                evaluation: eval,
                unmet,
            }));
        }

        if eval.status != target {
            info!(
                evaluation_id = %eval.id,
                from = ?eval.status,
                to = ?target,
                workers = project_workers.connected,
                eval_workers = project_workers.eval,
                network_workers = project_workers.network,
                "refreshing evaluation waiting state"
            );
        }

        if let (Some(project), Some(gap)) =
            (project, opened_gap(reason.as_ref(), new_reason.as_ref()))
        {
            announce_worker_gap(self.db, eval.id, project, gap).await;
        }

        persist_waiting_reason(
            &self.db.worker_db,
            eval.id,
            &eval.waiting_reason,
            new_reason.as_ref(),
        )
        .await;

        if eval.status != target {
            update_evaluation_status(self.db, eval, target).await?;
        }

        Ok(None)
    }

    async fn refresh_waiting_evaluation_cache_state(
        &self,
        eval: &MEvaluation,
        reason: Option<&WaitingReason>,
        project: Option<ProjectId>,
        workers: ProjectWorkerCounts,
        has_writable_cache_by_project: &mut HashMap<ProjectId, bool>,
    ) -> Result<Option<(EvaluationStatus, Option<WaitingReason>)>> {
        let parked_for_cache = matches!(reason, Some(WaitingReason::NoCache));
        let waits_before_evaluation = eval.status == EvaluationStatus::Queued
            || parked_for_cache
            || matches!(
                reason,
                Some(WaitingReason::EvalWorkers { capability, .. }) if *capability != EvalCapability::Exec
            );
        if !waits_before_evaluation {
            return Ok(None);
        }
        let Some(project) = project else {
            return Ok(parked_for_cache.then(|| (eval.status, reason.cloned())));
        };

        let has_writable_cache = match has_writable_cache_by_project.get(&project) {
            Some(known) => *known,
            None => {
                let found = gradient_db::projects::caches::project_has_writable_cache(
                    &self.db.worker_db,
                    project,
                )
                .await
                .context("check the project's writable cache")?;
                has_writable_cache_by_project.insert(project, found);
                found
            }
        };

        Ok(match (has_writable_cache, parked_for_cache) {
            (false, _) => Some((EvaluationStatus::Waiting, Some(WaitingReason::NoCache))),
            (true, true) => Some(
                decide_pre_build_target(
                    eval.kind,
                    EvaluationStatus::Queued,
                    check_repository_url_is_ssh(&eval.repository),
                    workers,
                )
                .unwrap_or((EvaluationStatus::Queued, None)),
            ),
            (true, false) => None,
        })
    }

    async fn exec_jobs_wait(
        &self,
        evaluation: EvaluationId,
        current: Option<&WaitingReason>,
    ) -> Result<Option<(EvaluationStatus, Option<WaitingReason>)>> {
        let waiting_exec_jobs =
            gradient_db::exec_jobs::count_waiting_exec_jobs(&self.db.worker_db, evaluation)
                .await
                .context("count waiting exec jobs")?;
        let next = (waiting_exec_jobs > 0).then_some(WaitingReason::ExecJobs { waiting_exec_jobs });
        let clears_stale = matches!(current, Some(WaitingReason::ExecJobs { .. }));
        Ok((next.is_some() || clears_stale).then_some((EvaluationStatus::Building, next)))
    }
}

pub(crate) async fn announce_worker_gap(
    db: &DbContext,
    evaluation: EvaluationId,
    project: ProjectId,
    reason: &WaitingReason,
) {
    let teams =
        match gradient_db::teams::workers::teams_granting_workers(&db.worker_db, project).await {
            Ok(teams) => teams,
            Err(e) => {
                warn!(error = %e, %evaluation, "failed to read teams granting workers");
                return;
            }
        };
    for event in worker_demand_events(reason, evaluation, project, &teams) {
        db.record(event).await;
    }
}

fn opened_gap<'a>(
    current: Option<&WaitingReason>,
    new: Option<&'a WaitingReason>,
) -> Option<&'a WaitingReason> {
    let new = new?;
    let gap = new.worker_gap()?;
    (current.and_then(WaitingReason::worker_gap) != Some(gap)).then_some(new)
}

fn worker_demand_events(
    reason: &WaitingReason,
    evaluation: EvaluationId,
    project: ProjectId,
    teams: &[TeamId],
) -> Vec<WorkerDemand> {
    teams
        .iter()
        .map(|&team| WorkerDemand {
            team,
            project,
            evaluation,
            reason: reason.clone(),
        })
        .collect()
}

struct WaitingRefreshPassSharedInputs<'a> {
    deadline: Instant,
    workers: &'a [WorkerInfo],
    tasks: EvalTasks,
    counters: HashMap<EvaluationId, EvalCounters>,
    now: chrono::NaiveDateTime,
    caps_by_project: HashMap<Option<ProjectId>, Vec<BuildWorkerCaps>>,
    has_writable_cache_by_project: HashMap<ProjectId, bool>,
}

#[derive(Default)]
struct EvalTasks {
    projects: HashMap<TaskId, ProjectId>,
    waiting_for_workers: HashSet<TaskId>,
}

impl EvalTasks {
    async fn load(db: &WorkerDb, evals: &[MEvaluation]) -> Result<Self> {
        let ids: HashSet<TaskId> = evals.iter().filter_map(|e| e.task).collect();
        if ids.is_empty() {
            return Ok(Self::default());
        }

        let rows: Vec<(TaskId, ProjectId, bool)> = ETask::find()
            .select_only()
            .columns([CTask::Id, CTask::Project, CTask::WaitForWorkers])
            .filter(CTask::Id.is_in(ids))
            .into_tuple()
            .all(db)
            .await
            .context("fetch tasks of in-flight evaluations")?;

        Ok(Self {
            projects: rows
                .iter()
                .map(|(id, project, _)| (*id, *project))
                .collect(),
            waiting_for_workers: rows
                .into_iter()
                .filter_map(|(id, _, waits)| waits.then_some(id))
                .collect(),
        })
    }

    fn project_of(&self, eval: &MEvaluation) -> Option<ProjectId> {
        eval.task.and_then(|t| self.projects.get(&t).copied())
    }

    fn wait_for_workers(&self, eval: &MEvaluation) -> bool {
        eval.task
            .is_some_and(|t| self.waiting_for_workers.contains(&t))
    }
}

impl WaitingRefresh<'_> {
    async fn build_phase_decision(
        &self,
        evaluation_id: EvaluationId,
        counters: EvalCounters,
        worker_caps: &[BuildWorkerCaps],
        current: Option<&WaitingReason>,
        deadline: Instant,
    ) -> Result<BuildPhase> {
        let a = match assess_buildability(
            &self.db.worker_db,
            Some(self.memo),
            evaluation_id,
            counters,
            worker_caps,
        )
        .await?
        {
            BuildPhase::Pending(a) => a,
            BuildPhase::Settled => {
                finalize_settled(self.db, evaluation_id).await;
                return Ok(BuildPhase::Settled);
            }
            BuildPhase::Unnamed => return Ok(BuildPhase::Unnamed),
        };

        if let (EvaluationStatus::Waiting, Some(WaitingReason::Workers { unmet, .. })) =
            (a.target, &a.reason)
            && unmet.is_empty()
        {
            if !unstick_due(current, a.pending) {
                return Ok(BuildPhase::Pending(Assessment {
                    target: EvaluationStatus::Waiting,
                    reason: Some(WaitingReason::graph_stuck(a.pending)),
                    pending: a.pending,
                }));
            }
            if Instant::now() >= deadline {
                // A graph stuck reason here can mark unstick as complete and later passes skip it.
                return Ok(BuildPhase::Pending(a));
            }

            return self.attempt_graph_unstick(evaluation_id, worker_caps).await;
        }

        Ok(BuildPhase::Pending(a))
    }
}

async fn finalize_settled(db: &DbContext, evaluation_id: EvaluationId) {
    if let Err(e) = gradient_db::status::check_evaluation_done(db, evaluation_id).await {
        error!(error = %e, %evaluation_id, "failed to finalize a settled evaluation");
    }
}

pub(crate) fn unstick_due(current: Option<&WaitingReason>, pending: u32) -> bool {
    !matches!(current, Some(WaitingReason::GraphStuck { pending_shared_builds }) if *pending_shared_builds == pending)
}

struct Assessment {
    target: EvaluationStatus,
    reason: Option<WaitingReason>,
    pending: u32,
}

enum BuildPhase {
    Unnamed,
    Settled,
    Pending(Assessment),
}

fn phase_from_counters(c: EvalCounters) -> Option<BuildPhase> {
    if c.named == 0 {
        return Some(BuildPhase::Unnamed);
    }

    if c.active == 0 {
        return Some(BuildPhase::Settled);
    }

    (c.building > 0).then_some(BuildPhase::Pending(Assessment {
        target: EvaluationStatus::Building,
        reason: None,
        pending: u32::try_from(c.active.max(0)).unwrap_or(u32::MAX),
    }))
}

fn lock(memo: &Mutex<AssessmentMemo>) -> std::sync::MutexGuard<'_, AssessmentMemo> {
    memo.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn assess_buildability(
    db: &WorkerDb,
    memo: Option<&Mutex<AssessmentMemo>>,
    evaluation_id: EvaluationId,
    counters: EvalCounters,
    worker_caps: &[BuildWorkerCaps],
) -> Result<BuildPhase> {
    if let Some(phase) = phase_from_counters(counters) {
        return Ok(phase);
    }

    let pending_count = u32::try_from(counters.active.max(0)).unwrap_or(u32::MAX);
    let now = Instant::now();
    if let Some((target, reason)) =
        memo.and_then(|m| lock(m).get(evaluation_id, counters, worker_caps, now))
    {
        return Ok(BuildPhase::Pending(Assessment {
            target,
            reason,
            pending: pending_count,
        }));
    }

    let pending = eval_blocking_shared_builds(db, evaluation_id).await?;
    if pending.is_empty() {
        gradient_db::evaluations::counters::recount_evaluations(db, &[evaluation_id])
            .await
            .context("recount counters the shared builds contradict")?;
        return Ok(BuildPhase::Settled);
    }

    let checker = BuildabilityChecker::load(db, &pending).await?;
    let target = if checker.any_buildable(&pending, worker_caps) {
        EvaluationStatus::Building
    } else {
        EvaluationStatus::Waiting
    };
    let reason = if matches!(target, EvaluationStatus::Waiting) {
        let reason = checker.compute_waiting_reason(&pending, worker_caps);
        Some(waiting_on_upstream_caches(db, evaluation_id, &pending, reason).await?)
    } else {
        None
    };

    if let Some(m) = memo {
        lock(m).put(
            evaluation_id,
            counters,
            worker_caps,
            now,
            target,
            reason.clone(),
        );
    }

    Ok(BuildPhase::Pending(Assessment {
        target,
        reason,
        pending: pending_count,
    }))
}

/// A shared build without an upstream answer cannot start, whatever workers are connected.
async fn waiting_on_upstream_caches(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    pending: &[MDerivationBuild],
    reason: WaitingReason,
) -> Result<WaitingReason> {
    let unanswered = pending.iter().filter(|a| !a.probed).count();
    if unanswered == 0
        || !matches!(&reason, WaitingReason::Workers { unmet, .. } if unmet.is_empty())
    {
        return Ok(reason);
    }

    let failing =
        gradient_db::caches::upstream::failing_upstreams_for_evaluation(db, evaluation_id)
            .await
            .context("read the upstream caches failing to answer")?;
    Ok(WaitingReason::upstream_caches(
        u32::try_from(unanswered).unwrap_or(u32::MAX),
        failing,
    ))
}

impl WaitingRefresh<'_> {
    async fn attempt_graph_unstick(
        &self,
        evaluation_id: EvaluationId,
        worker_caps: &[BuildWorkerCaps],
    ) -> Result<BuildPhase> {
        info!(%evaluation_id, "graph stuck: pool can build every pending shared build but none is dispatchable; self-healing");

        if let Err(e) = self
            .graph
            .transition(gradient_graph::Transition::Repair {
                scope: gradient_db::graph::repair::RepairScope::Unstick(evaluation_id),
            })
            .await
        {
            error!(error = %e, %evaluation_id, "unstick repair did not reach the graph writer");
        }

        let counters =
            gradient_db::evaluations::counters::eval_counters(&self.db.worker_db, evaluation_id)
                .await
                .context("read evaluation shared build counters after the heal")?
                .unwrap_or_default();
        let blocked = match assess_buildability(
            &self.db.worker_db,
            None,
            evaluation_id,
            counters,
            worker_caps,
        )
        .await?
        {
            BuildPhase::Pending(a) if a.target == EvaluationStatus::Building => {
                return Ok(BuildPhase::Pending(a));
            }
            BuildPhase::Pending(a) => a.pending,
            BuildPhase::Settled => {
                finalize_settled(self.db, evaluation_id).await;
                return Ok(BuildPhase::Settled);
            }
            BuildPhase::Unnamed => return Ok(BuildPhase::Unnamed),
        };

        Ok(BuildPhase::Pending(Assessment {
            target: EvaluationStatus::Waiting,
            reason: Some(WaitingReason::graph_stuck(blocked)),
            pending: blocked,
        }))
    }
}

/// [`refresh_waiting_state`] is starting the heal only on entry and on a change of the pending set.
/// `repair_cached_shared_builds_for_eval` and `repair_dependency_failed` have no other driver for a
/// stably stuck evaluation. The can-start recount cannot stand in, because `fetchable` is reading
/// the very status that heal is fixing.
pub async fn reheal_graph_stuck_evals(db: &WorkerDb, graph: &Graph) -> Result<()> {
    let waiting = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(db)
        .await
        .context("fetch waiting evaluations")?;

    let mut stuck = 0u32;
    let mut healed = 0u32;
    for eval in waiting {
        let graph_stuck = eval
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json)
            .is_some_and(|r| matches!(r, WaitingReason::GraphStuck { .. }));
        if !graph_stuck {
            continue;
        }
        stuck += 1;

        if let Err(e) = graph
            .transition(gradient_graph::Transition::Repair {
                scope: gradient_db::graph::repair::RepairScope::Unstick(eval.id),
            })
            .await
        {
            error!(error = %e, evaluation_id = %eval.id, "graph-stuck re-heal did not reach the graph writer");
            continue;
        }
        healed += 1;
    }

    if stuck > 0 {
        info!(stuck, healed, "re-healed graph-stuck evaluations");
    }

    Ok(())
}

/// A build target's own `.drv` is producerless, and the daemon-free server cannot reproduce one. A
/// fresh evaluation of the same commit is the sole recovery. A `DrvRecovery` run stalling the same
/// way is failed, not retried, to avoid a loop.
pub async fn recover_drv_stuck_evals(scheduler: &crate::Scheduler) -> Result<()> {
    let db = &scheduler.db;
    let waiting = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(&db.worker_db)
        .await
        .context("fetch waiting evaluations")?;

    let now = gradient_types::now();
    for eval in waiting {
        let graph_stuck = eval
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json)
            .is_some_and(|r| matches!(r, WaitingReason::GraphStuck { .. }));
        if !graph_stuck || (now - eval.updated_at).num_seconds() < DRV_RECOVERY_GRACE_SECS {
            continue;
        }
        if !eval_blocked_on_unproducible_drv(&db.worker_db, eval.id).await? {
            continue;
        }

        if eval.kind == EvaluationKind::DrvRecovery {
            warn!(evaluation_id = %eval.id, "drv-recovery re-eval still blocked on its own missing .drv; failing (unrecoverable)");
            update_evaluation_status(db, eval, EvaluationStatus::Failed).await?;
            continue;
        }

        let Some(task_id) = eval.task else {
            continue;
        };
        let task = match by_id::<ETask, _>(&db.worker_db, task_id).await {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                warn!(evaluation_id = %eval.id, error = %e, "drv recovery: task lookup failed");
                continue;
            }
        };

        match abort_stuck_evaluation_and_insert_recovery_in_transaction(&db.worker_db, &task, &eval)
            .await
        {
            Ok(new_eval) => {
                scheduler.stop_aborted_evaluation(eval.id, false).await;
                info!(stuck = %eval.id, recovery = %new_eval.id, "auto-triggered .drv-recovery re-evaluation");
                db.record_evaluation_created(&new_eval).await;
            }
            Err(e) => {
                warn!(evaluation_id = %eval.id, error = %e, "failed to trigger .drv recovery")
            }
        }
    }

    Ok(())
}

async fn abort_stuck_evaluation_and_insert_recovery_in_transaction(
    db: &WorkerDb,
    task: &MTask,
    stuck: &MEvaluation,
) -> Result<MEvaluation> {
    let txn = db.begin().await?;
    let recovery = gradient_ci::trigger_drv_recovery(&txn, task, stuck).await?;
    txn.commit().await?;
    Ok(recovery)
}

async fn eval_blocked_on_unproducible_drv(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
) -> Result<bool> {
    let row = db
        .query_one_raw(UNPRODUCIBLE_DRV_BLOCK.bind([Value::Uuid(Some(evaluation_id.into_inner()))]))
        .await
        .context("detect unproducible-drv block")?;

    Ok(row
        .and_then(|r| r.try_get::<bool>("", "blocked").ok())
        .unwrap_or(false))
}

gradient_db::sql_fn! {
    UNPRODUCIBLE_DRV_BLOCK = unproducible_drv_block_sql,
        params = [EvaluationId];
}

fn unproducible_drv_block_sql() -> String {
    let drv_nar_absent = gradient_db::graph::predicates::drv_nar_absent_predicate("db");
    let walked = gradient_db::graph::predicates::walked_predicate("db");
    format!(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM build_job bj
            JOIN derivation_build db ON db.id = bj.derivation_build
            WHERE bj.evaluation = $1
              AND db.status IN ({created}, {queued})
              AND db.wanted
              AND {walked}
              AND NOT db.cache_available
              AND {drv_nar_absent}
              AND db.blocking_deps = 0
        ) AS blocked
        "#,
        created = BuildStatus::Created as i32,
        queued = BuildStatus::Queued as i32,
    )
}

async fn eval_blocking_shared_builds(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
) -> Result<Vec<MDerivationBuild>> {
    use sea_orm::sea_query::Query;
    let named = Query::select()
        .column(CBuildJob::DerivationBuild)
        .from(EBuildJob::default())
        .and_where(CBuildJob::Evaluation.eq(evaluation_id))
        .to_owned();
    let pending = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.in_subquery(named))
        .filter(CDerivationBuild::Status.is_in(gradient_db::graph::predicates::NEED_BUILD_STATUSES))
        .all(db)
        .await
        .context("fetch pending shared builds")?;

    Ok(pending
        .into_iter()
        .filter(|a| gradient_db::graph::predicates::blocks_evaluation(a.status, a.wanted))
        .collect())
}

fn pre_build_capability(
    kind: EvaluationKind,
    status: EvaluationStatus,
    ssh_repository: bool,
) -> Option<EvalCapability> {
    if kind == EvaluationKind::Ssh {
        return None;
    }

    match status {
        EvaluationStatus::Fetching if ssh_repository => Some(EvalCapability::Secrets),
        EvaluationStatus::Fetching => Some(EvalCapability::Network),
        EvaluationStatus::Queued
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation => Some(EvalCapability::Eval),
        _ => None,
    }
}

fn eligible_for(worker: &WorkerInfo, project: Option<ProjectId>) -> bool {
    match project {
        Some(project) => worker.project_access.contains(&project),
        None => worker.project_access == ProjectAccess::AllProjects,
    }
}

fn project_worker_caps(workers: &[WorkerInfo], project: Option<ProjectId>) -> Vec<BuildWorkerCaps> {
    workers
        .iter()
        .filter(|w| eligible_for(w, project))
        .map(BuildWorkerCaps::from)
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProjectWorkerCounts {
    eval: usize,
    network: usize,
    secrets: usize,
    exec: usize,
    connected: u32,
}

impl ProjectWorkerCounts {
    fn of(workers: &[WorkerInfo], project: Option<ProjectId>) -> Self {
        workers
            .iter()
            .filter(|w| eligible_for(w, project))
            .fold(Self::default(), |counts, w| Self {
                eval: counts.eval + usize::from(w.capabilities.eval),
                network: counts.network + usize::from(w.capabilities.network),
                secrets: counts.secrets
                    + usize::from(w.capabilities.network && w.capabilities.secrets),
                exec: counts.exec + usize::from(w.capabilities.exec),
                connected: counts.connected + 1,
            })
    }

    fn provide(self, capability: EvalCapability) -> bool {
        match capability {
            EvalCapability::Network => self.network > 0,
            EvalCapability::Secrets => self.secrets > 0,
            EvalCapability::Eval => self.eval > 0,
            EvalCapability::Exec => self.exec > 0,
        }
    }
}

fn decide_pre_build_target(
    kind: EvaluationKind,
    current: EvaluationStatus,
    ssh_repository: bool,
    workers: ProjectWorkerCounts,
) -> Option<(EvaluationStatus, Option<WaitingReason>)> {
    let capability = pre_build_capability(kind, current, ssh_repository)?;
    if workers.provide(capability) {
        return None;
    }

    Some((
        EvaluationStatus::Waiting,
        Some(WaitingReason::eval_workers(capability, workers.connected)),
    ))
}

fn decide_eval_recovery(
    capability: EvalCapability,
    workers: ProjectWorkerCounts,
) -> (EvaluationStatus, Option<WaitingReason>) {
    if workers.provide(capability) {
        (EvaluationStatus::Queued, None)
    } else {
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(capability, workers.connected)),
        )
    }
}

pub(crate) async fn persist_waiting_reason(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    current: &Option<serde_json::Value>,
    new_reason: Option<&WaitingReason>,
) {
    let new_value = new_reason.map(|r| r.to_json());

    let unchanged = match (current, &new_value) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if unchanged {
        return;
    }

    let res = EEvaluation::update_many()
        .col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(new_value),
        )
        .filter(CEvaluation::Id.eq(evaluation_id))
        .exec(db)
        .await;

    if let Err(e) = res {
        warn!(error = %e, %evaluation_id, "failed to persist waiting_reason");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeDatabaseErrorWithStatementLiteralInMessage;

    impl std::fmt::Display for FakeDatabaseErrorWithStatementLiteralInMessage {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(sea_orm::sqlx::error::DatabaseError::message(self))
        }
    }

    impl std::error::Error for FakeDatabaseErrorWithStatementLiteralInMessage {}

    impl sea_orm::sqlx::error::DatabaseError for FakeDatabaseErrorWithStatementLiteralInMessage {
        fn message(&self) -> &str {
            "invalid input syntax for type uuid: \"secret-literal-of-statement\""
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some("22P02".into())
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> sea_orm::sqlx::error::ErrorKind {
            sea_orm::sqlx::error::ErrorKind::Other
        }
    }

    #[test]
    fn database_error_kind_and_sqlstate_warn_log_does_not_contain_postgres_message() {
        let db_err = sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(std::sync::Arc::new(
            sea_orm::sqlx::Error::Database(Box::new(
                FakeDatabaseErrorWithStatementLiteralInMessage,
            )),
        )));
        let err = anyhow::Error::new(db_err);
        assert!(
            format!("{err:#}").contains("secret-literal-of-statement"),
            "the full error text quotes the literal"
        );

        let (kind, sqlstate) = database_error_kind_text_and_sqlstate_for_warning_log(&err);
        assert_eq!(kind, "error for database query");
        assert_eq!(sqlstate.as_deref(), Some("22P02"));
        assert!(
            !format!("{kind} {sqlstate:?}").contains("secret-literal-of-statement"),
            "the warn summary leaves out the literal"
        );
    }

    fn eval_workers_view(r: &WaitingReason) -> (EvalCapability, u32) {
        match r {
            WaitingReason::EvalWorkers {
                capability,
                connected_workers,
            } => (*capability, *connected_workers),
            other => panic!("expected EvalWorkers variant, got {other:?}"),
        }
    }

    fn counts(eval: usize, network: usize, connected: u32) -> ProjectWorkerCounts {
        ProjectWorkerCounts {
            eval,
            network,
            secrets: network,
            exec: 0,
            connected,
        }
    }

    fn worker(eval: bool, authorized_peers: Option<HashSet<ProjectId>>) -> WorkerInfo {
        WorkerInfo {
            id: "00000000-0000-4000-8000-000000000001".into(),
            capabilities: gradient_wire::types::GradientCapabilities {
                eval,
                network: eval,
                secrets: eval,
                ..Default::default()
            },
            architectures: vec![],
            system_features: vec![],
            max_concurrent_builds: 1,
            assigned_job_count: 0,
            assigned_build_count: 0,
            draining: false,
            project_access: authorized_peers.map_or(
                ProjectAccess::AllProjects,
                ProjectAccess::OnlyListedProjects,
            ),
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            upload_speed_mbps: None,
            download_speed_mbps: None,
            zone: None,
            exec: None,
        }
    }

    fn capable_worker(
        architectures: &[&str],
        features: &[&str],
        authorized_peers: Option<HashSet<ProjectId>>,
    ) -> WorkerInfo {
        let mut base = worker(true, authorized_peers);
        base.capabilities.build = true;
        WorkerInfo {
            architectures: architectures.iter().map(|a| a.to_string()).collect(),
            system_features: features.iter().map(|f| f.to_string()).collect(),
            ..base
        }
    }

    fn x86_gap(connected_workers: u32) -> WaitingReason {
        WaitingReason::workers(
            vec![UnmetRequirement {
                architecture: "x86_64-linux".into(),
                required_features: vec!["kvm".into()],
                build_count: connected_workers + 1,
            }],
            connected_workers,
            vec![],
        )
    }

    #[test]
    fn reason_change_records_one_event_per_granting_team() {
        let new = x86_gap(0);
        let gap =
            opened_gap(Some(&WaitingReason::Draining), Some(&new)).expect("a new worker gap opens");
        let teams = [TeamId::now_v7(), TeamId::now_v7()];
        let events = worker_demand_events(gap, EvaluationId::now_v7(), ProjectId::now_v7(), &teams);
        assert_eq!(
            events.iter().map(|e| e.team).collect::<Vec<_>>(),
            teams.to_vec()
        );
        assert_ne!(
            gradient_types::events::EventKind::key(&events[0]),
            gradient_types::events::EventKind::key(&events[1])
        );
    }

    #[test]
    fn unchanged_reason_records_nothing() {
        assert!(opened_gap(Some(&x86_gap(1)), Some(&x86_gap(3))).is_none());
        assert!(opened_gap(None, None).is_none());
        assert!(opened_gap(None, Some(&WaitingReason::graph_stuck(2))).is_none());
    }

    #[test]
    fn worker_restricted_to_other_project_gives_no_caps() {
        let (own, other) = (ProjectId::now_v7(), ProjectId::now_v7());
        let w = capable_worker(&["aarch64-darwin"], &[], Some(HashSet::from([other])));
        assert!(project_worker_caps(&[w], Some(own)).is_empty());
    }

    #[test]
    fn open_worker_gives_caps_to_every_project() {
        let w = capable_worker(&["x86_64-linux"], &["kvm"], None);
        assert_eq!(
            project_worker_caps(&[w], Some(ProjectId::now_v7())),
            vec![BuildWorkerCaps {
                architectures: vec!["x86_64-linux".to_string()],
                features: vec!["kvm".to_string()],
                network: true,
                build: true,
            }]
        );
    }

    #[test]
    fn a_queued_evaluation_waits_when_only_other_projects_have_eval_workers() {
        let (own, other) = (ProjectId::now_v7(), ProjectId::now_v7());
        let workers = [worker(true, Some(HashSet::from([other])))];

        let (target, reason) = decide_pre_build_target(
            EvaluationKind::Normal,
            EvaluationStatus::Queued,
            false,
            ProjectWorkerCounts::of(&workers, Some(own)),
        )
        .expect("a project without its own eval worker must stall");
        assert_eq!(target, EvaluationStatus::Waiting);
        assert_eq!(
            eval_workers_view(&reason.expect("stall carries a reason")),
            (EvalCapability::Eval, 0)
        );
    }

    #[test]
    fn project_worker_counts_include_open_and_authorized_workers_only() {
        let (own, other) = (ProjectId::now_v7(), ProjectId::now_v7());
        let workers = [
            worker(true, None),
            worker(true, Some(HashSet::from([own]))),
            worker(false, Some(HashSet::from([own, other]))),
            worker(true, Some(HashSet::from([other]))),
        ];

        assert_eq!(
            ProjectWorkerCounts::of(&workers, Some(own)),
            counts(2, 2, 3)
        );
        assert_eq!(ProjectWorkerCounts::of(&workers, None), counts(1, 1, 1));
    }

    #[test]
    fn pre_build_target_queued_no_eval_worker_stalls_to_eval_waiting() {
        let (target, reason) = decide_pre_build_target(
            EvaluationKind::Normal,
            EvaluationStatus::Queued,
            false,
            counts(0, 1, 3),
        )
        .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("stall carries a reason"));
        assert_eq!(cap, EvalCapability::Eval);
        assert_eq!(connected, 3);
    }

    #[test]
    fn pre_build_target_fetching_without_network_waits_for_network() {
        let (target, reason) = decide_pre_build_target(
            EvaluationKind::Normal,
            EvaluationStatus::Fetching,
            false,
            counts(2, 0, 2),
        )
        .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("stall carries a reason"));
        assert_eq!(cap, EvalCapability::Network);
        assert_eq!(connected, 2);
    }

    #[test]
    fn fetching_an_ssh_repository_waits_for_secrets() {
        let workers = ProjectWorkerCounts {
            secrets: 0,
            ..counts(2, 2, 2)
        };
        let (_, reason) = decide_pre_build_target(
            EvaluationKind::Normal,
            EvaluationStatus::Fetching,
            true,
            workers,
        )
        .expect("an ssh repository needs a worker with secrets");
        assert_eq!(
            eval_workers_view(&reason.expect("stall carries a reason")).0,
            EvalCapability::Secrets
        );
        assert!(
            decide_pre_build_target(
                EvaluationKind::Normal,
                EvaluationStatus::Fetching,
                false,
                workers,
            )
            .is_none(),
            "a public repository needs only network"
        );
    }

    #[test]
    fn pre_build_target_active_pre_build_with_capability_left_alone() {
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Queued,
        ] {
            assert!(
                decide_pre_build_target(EvaluationKind::Normal, status, false, counts(1, 1, 1))
                    .is_none(),
                "{status:?} with capable workers must be left alone"
            );
        }
    }

    #[test]
    fn an_ssh_evaluation_records_its_derivations_without_eval_workers() {
        assert!(
            decide_pre_build_target(
                EvaluationKind::Ssh,
                EvaluationStatus::EvaluatingDerivation,
                false,
                counts(0, 0, 0)
            )
            .is_none()
        );
    }

    #[test]
    fn pre_build_target_ignores_waiting() {
        assert!(
            decide_pre_build_target(
                EvaluationKind::Normal,
                EvaluationStatus::Waiting,
                false,
                counts(0, 0, 0)
            )
            .is_none()
        );
        assert!(
            decide_pre_build_target(
                EvaluationKind::Normal,
                EvaluationStatus::Waiting,
                false,
                counts(2, 2, 2)
            )
            .is_none()
        );
    }

    #[test]
    fn eval_recovery_unparks_to_queued_when_capability_returns() {
        let (target, reason) = decide_eval_recovery(EvalCapability::Eval, counts(1, 0, 1));
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());

        let (target, reason) = decide_eval_recovery(EvalCapability::Network, counts(0, 1, 1));
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());
    }

    #[test]
    fn eval_recovery_refreshes_reason_while_capability_absent() {
        let (target, reason) = decide_eval_recovery(EvalCapability::Network, counts(5, 0, 5));
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("refresh carries a reason"));
        assert_eq!(cap, EvalCapability::Network);
        assert_eq!(connected, 5);
    }

    #[test]
    fn counters_decide_without_shared_builds() {
        let c = |named, active, building| gradient_db::evaluations::counters::EvalCounters {
            named,
            active,
            building,
            ..Default::default()
        };
        assert!(matches!(
            phase_from_counters(c(0, 0, 0)),
            Some(BuildPhase::Unnamed)
        ));
        assert!(matches!(
            phase_from_counters(c(4, 0, 0)),
            Some(BuildPhase::Settled)
        ));
        assert!(matches!(
            phase_from_counters(c(4, 2, 1)),
            Some(BuildPhase::Pending(Assessment {
                target: EvaluationStatus::Building,
                reason: None,
                pending: 2
            }))
        ));
        assert!(phase_from_counters(c(4, 2, 0)).is_none());
    }

    #[test]
    fn the_graph_stuck_heal_runs_on_entry_and_on_change_only() {
        assert!(unstick_due(None, 3));
        assert!(unstick_due(
            Some(&WaitingReason::Workers {
                unmet: vec![],
                connected_workers: 1,
                available_architectures: vec![],
            }),
            3
        ));
        assert!(!unstick_due(Some(&WaitingReason::graph_stuck(3)), 3));
        assert!(unstick_due(Some(&WaitingReason::graph_stuck(3)), 2));
    }

    #[test]
    fn waiting_refresh_pass_starts_after_last_refreshed_evaluation_and_wraps_to_smallest_id() {
        let mut sorted: Vec<EvaluationId> = (0..3).map(|_| EvaluationId::now_v7()).collect();
        sorted.sort();
        let (first, second, third) = (sorted[0], sorted[1], sorted[2]);
        let rotated = |mut ids: Vec<EvaluationId>, last_refreshed| {
            rotate_evaluations_after_last_refreshed(&mut ids, last_refreshed, |id| *id);
            ids
        };

        assert_eq!(
            rotated(sorted.clone(), None),
            vec![first, second, third],
            "Nothing refreshed yet. The smallest id is first in the pass order."
        );
        assert_eq!(
            rotated(sorted.clone(), Some(first)),
            vec![second, third, first],
            "Begin the pass after the last refreshed id. Wrap around at the end."
        );
        assert_eq!(
            rotated(sorted.clone(), Some(third)),
            vec![first, second, third],
            "Refresh passes move back to the start after the last id. They begin with the smallest id."
        );
        assert_eq!(
            rotated(vec![first, third], Some(second)),
            vec![third, first],
            "The last refreshed evaluation is no longer in the set. Refresh passes start from the next higher ID."
        );
    }

    #[derive(Clone, Default)]
    pub(crate) struct TestLogEventRecorder(
        std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>,
    );

    impl TestLogEventRecorder {
        pub(crate) fn count(&self, level: tracing::Level, message: &str) -> usize {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(l, m)| *l == level && m == message)
                .count()
        }
    }

    impl tracing::Subscriber for TestLogEventRecorder {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), message.0));
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn build_invalid_text_representation_query_error_with_literal() -> anyhow::Error {
        anyhow::Error::new(sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(
            std::sync::Arc::new(sea_orm::sqlx::Error::Database(Box::new(
                FakeDatabaseErrorWithStatementLiteralInMessage,
            ))),
        )))
    }

    const FAILURE_WARNING: &str =
        "failed to update waiting state of one evaluation. proceed to next evaluation";
    const REPEATED_FAILURE: &str =
        "update of the waiting state for an evaluation failed again with the same error";

    #[test]
    fn waiting_refresh_failure_streak_logs_warn_then_debug_and_resets_on_error_change_or_success() {
        let events = TestLogEventRecorder::default();
        let evaluation_id = EvaluationId::now_v7();
        let mut warned = WaitingRefreshWarningRecord::default();

        tracing::subscriber::with_default(events.clone(), || {
            for _ in 0..3 {
                warned.log_waiting_refresh_failure(
                    evaluation_id,
                    &build_invalid_text_representation_query_error_with_literal(),
                );
            }
        });
        assert_eq!(
            events.count(tracing::Level::WARN, FAILURE_WARNING),
            1,
            "1 WARN line after 3 failed attempts"
        );
        assert_eq!(
            events.count(tracing::Level::DEBUG, REPEATED_FAILURE),
            2,
            "Three failing passes produce two DEBUG lines for the repetition."
        );

        tracing::subscriber::with_default(events.clone(), || {
            warned.log_waiting_refresh_failure(
                evaluation_id,
                &anyhow::anyhow!("graph writer stopped"),
            );
        });
        assert_eq!(
            events.count(tracing::Level::WARN, FAILURE_WARNING),
            2,
            "Second WARN line after an error change"
        );

        tracing::subscriber::with_default(events.clone(), || {
            warned.clear_warned_error(evaluation_id);
            warned.log_waiting_refresh_failure(
                evaluation_id,
                &anyhow::anyhow!("graph writer stopped"),
            );
        });
        assert_eq!(
            events.count(tracing::Level::WARN, FAILURE_WARNING),
            3,
            "Same failure after success: WARN line again"
        );
        assert_eq!(
            events.count(tracing::Level::DEBUG, REPEATED_FAILURE),
            2,
            "Do not output a repeated DEBUG line after a changed error or after a success."
        );
    }

    #[test]
    fn evaluation_no_longer_running_is_removed_from_warned_failures() {
        let (kept, gone) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let mut warned = WaitingRefreshWarningRecord::default();

        tracing::subscriber::with_default(TestLogEventRecorder::default(), || {
            warned.log_waiting_refresh_failure(
                kept,
                &build_invalid_text_representation_query_error_with_literal(),
            );
            warned.log_waiting_refresh_failure(
                gone,
                &build_invalid_text_representation_query_error_with_literal(),
            );
        });
        warned.retain(&HashSet::from([kept]));

        assert!(
            warned
                .last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id
                .contains_key(&kept),
            "In-flight evaluations retain the warned error."
        );
        assert!(
            !warned
                .last_waiting_refresh_warning_error_kind_and_sqlstate_by_evaluation_id
                .contains_key(&gone),
            "Evaluations that leave the in-flight set also leave the map."
        );
    }

    #[test]
    fn waiting_refresh_over_budget_streak_logs_info_then_debug_and_pass_within_budget_ends_streak()
    {
        const WAITING_REFRESH_PASS_TIME_BUDGET_REACHED_LOG_MESSAGE: &str =
            "waiting refresh exhausted time budget. skipped evaluations come first on next pass";
        let events = TestLogEventRecorder::default();
        let mut progress = WaitingRefreshStateBetweenPasses::default();
        let lines = |events: &TestLogEventRecorder| {
            (
                events.count(
                    tracing::Level::INFO,
                    WAITING_REFRESH_PASS_TIME_BUDGET_REACHED_LOG_MESSAGE,
                ),
                events.count(
                    tracing::Level::DEBUG,
                    WAITING_REFRESH_PASS_TIME_BUDGET_REACHED_LOG_MESSAGE,
                ),
            )
        };

        tracing::subscriber::with_default(events.clone(), || {
            progress.log_waiting_refresh_budget_stop(1, 2)
        });
        assert_eq!(lines(&events), (1, 0), "after pass 1 spent: one INFO line");

        tracing::subscriber::with_default(events.clone(), || {
            progress.log_waiting_refresh_budget_stop(1, 2)
        });
        assert_eq!(
            lines(&events),
            (1, 1),
            "Pass 2 used up. One DEBUG line, no new INFO line."
        );

        tracing::subscriber::with_default(events.clone(), || {
            progress.record_waiting_refresh_pass_within_budget()
        });
        assert_eq!(
            lines(&events),
            (1, 1),
            "Refresh pass 3 finished within budget. No budget line."
        );

        tracing::subscriber::with_default(events.clone(), || {
            progress.log_waiting_refresh_budget_stop(1, 2)
        });
        assert_eq!(
            lines(&events),
            (2, 1),
            "after pass 4 spent: an INFO line again"
        );
    }
}
