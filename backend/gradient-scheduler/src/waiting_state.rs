/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::by_id;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};

use gradient_db::status::update_evaluation_status;
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_graph::Graph;
use gradient_pool::WorkerInfo;
use gradient_types::events::worker_demand::WorkerDemand;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Value};
use tracing::{error, info, warn};

const DRV_RECOVERY_GRACE_SECS: i64 = 120;

use crate::assessment_memo::AssessmentMemo;
use crate::buildability::{BuildWorkerCaps, BuildabilityChecker};
use crate::unbuildable::{Unbuildable, unbuildable};
use gradient_db::evaluations::counters::EvalCounters;

pub(crate) struct WaitingRefresh<'a> {
    pub(crate) db: &'a DbContext,
    pub(crate) graph: &'a Graph,
    pub(crate) memo: &'a Mutex<AssessmentMemo>,
}

impl WaitingRefresh<'_> {
    pub(crate) async fn run(
        &self,
        workers: &[WorkerInfo],
        draining: bool,
    ) -> Result<Vec<Unbuildable>> {
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
        let mut caps_by_project: HashMap<Option<ProjectId>, Vec<BuildWorkerCaps>> = HashMap::new();
        let now = gradient_types::now();
        let mut unbuildables = Vec::new();
        let ids: Vec<EvaluationId> = evals.iter().map(|e| e.id).collect();
        let counters =
            gradient_db::evaluations::counters::in_flight_counters(&self.db.worker_db, &ids)
                .await
                .context("read evaluation shared build counters")?;
        lock(self.memo).retain(&ids.iter().copied().collect::<HashSet<_>>());

        for eval in evals {
            let eval_counters = counters.get(&eval.id).copied().unwrap_or_default();
            let reason = eval
                .waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json);
            let project = tasks.project_of(&eval);
            let project_workers = ProjectWorkerCounts::of(workers, project);
            let worker_caps = caps_by_project
                .entry(project)
                .or_insert_with(|| project_worker_caps(workers, project))
                .as_slice();

            if eval.status == EvaluationStatus::Waiting
                && reason.as_ref().is_some_and(|r| {
                    matches!(
                        r,
                        WaitingReason::Approval { .. }
                            | WaitingReason::NoCache
                            | WaitingReason::CacheStorageFull
                    )
                })
            {
                continue;
            }

            let outcome = match eval.status {
                EvaluationStatus::Waiting => match reason {
                    Some(WaitingReason::EvalWorkers { capability, .. }) => {
                        Some(decide_eval_recovery(capability, project_workers))
                    }
                    Some(WaitingReason::Draining) => Some((eval.status_after_draining(), None)),
                    _ => match self
                        .build_phase_decision(eval.id, eval_counters, worker_caps, reason.as_ref())
                        .await?
                    {
                        BuildPhase::Pending(a) => Some((a.target, a.reason)),
                        BuildPhase::Settled => continue,
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
                        .build_phase_decision(eval.id, eval_counters, worker_caps, reason.as_ref())
                        .await?
                    {
                        BuildPhase::Pending(a) => Some((a.target, a.reason)),
                        BuildPhase::Settled | BuildPhase::Unnamed => None,
                    }
                }
                _ => None,
            };

            let Some((target, new_reason)) = outcome else {
                continue;
            };

            let waits = tasks.wait_for_workers(&eval);
            if let Some(unmet) =
                unbuildable(&eval, reason.as_ref(), new_reason.as_ref(), waits, now)
            {
                unbuildables.push(Unbuildable {
                    evaluation: eval,
                    unmet,
                });
                continue;
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
        }

        Ok(unbuildables)
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

        match gradient_ci::trigger_drv_recovery(&db.worker_db, &task, &eval).await {
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
    worker
        .authorized_peers
        .as_ref()
        .is_none_or(|peers| project.is_some_and(|p| peers.contains(&p)))
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
mod tests {
    use super::*;

    #[test]
    fn unproducible_drv_block_sql_shape() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(unproducible_drv_block_sql());
        assert!(
            sql.contains(&format!(
                "db.status IN ({}, {})",
                BuildStatus::Created as i32,
                BuildStatus::Queued as i32
            )),
            "only pending shared builds: {sql}"
        );
        for frag in ["db.wanted", "w.walked", "NOT db.cache_available"] {
            assert!(sql.contains(frag), "missing `{frag}`: {sql}");
        }
        assert!(
            sql.contains(&norm(
                gradient_db::graph::predicates::drv_nar_absent_predicate("db")
            )),
            "must require the .drv's own NAR to be absent, through the shared predicate: {sql}"
        );
        assert!(
            sql.contains("db.blocking_deps = 0"),
            "must require every dependency fetchable, through the counter: {sql}"
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
            authorized_peers,
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            upload_speed_mbps: None,
            download_speed_mbps: None,
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
}
