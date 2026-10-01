/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::assign_mode::decide_build_spec_kind;
use gradient_core::ServerState;
use gradient_db::StartableMoves;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_graph::{RequeueScope, Transition};
use gradient_sources::get_path_from_derivation_output;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use gradient_util::supervision::{ChildCtx, ChildSpec, run_pass};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, warn};

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::PendingBuildJob;
use gradient_wire::types::{
    BuildJob, BuildSpec, BuildSpecKind, CacheInfo, DerivationOutput, RequiredPath,
};

use super::{ASSIGN_BUDGET, ASSIGN_TICK, STARTABLE_RESYNC};

/// One dispatch pass. A timer tick also advances the rescore clock; a kick starts
/// only the dispatch half. Neither repairs: every can-start counter is moved by
/// the event that changes it, and the two remaining scopes are evaluation-driven.
/// Every pass admits what moved; only `resync` reads the whole startable set.
pub(crate) async fn build_assign_pass(scheduler: &Scheduler, timer_tick: bool, resync: bool) {
    // rescore_count is an anti-starvation timeout measured in dispatch
    // intervals, so only the timer advances it - reactive kicks (which can
    // fire many times per interval) just run an extra dispatch pass.
    if timer_tick {
        let _ = scheduler
            .call(|reply| SchedulerMsg::BumpRescore { reply })
            .await;
    }

    if let Err(e) = scheduler
        .state
        .graph
        .requeue(RequeueScope::TransientRetries)
        .await
    {
        error!(error = %e, "transient retry requeue did not reach the graph writer");
    }
    if let Err(e) = admit_startable_moves(scheduler).await {
        error!(error = %e, "startable-set admission error");
    }
    if resync && let Err(e) = resync_startable_set(scheduler).await {
        error!(error = %e, "startable-set resync error");
    }
    // After dispatching, refresh each in-flight evaluation's
    // Building/Waiting state so the UI reflects "no worker can pick this
    // up" (or recovers when a worker comes back online). Cheap when
    // there are no in-flight evals.
    if let Err(e) = scheduler.refresh_waiting_state().await {
        error!(error = %e, "refresh_waiting_state in dispatch loop failed");
    }
    // A `.drv` our cache lost has no producer; the only recovery is a fresh
    // evaluation of the same commit. Executes after the waiting-state refresh
    // has settled `graph_stuck`.
    if let Err(e) = crate::waiting_state::recover_drv_stuck_evals(&scheduler.state).await {
        error!(error = %e, "recover_drv_stuck_evals in dispatch loop failed");
    }

    // Re-offer still-pending jobs to all sessions each pass. A build
    // re-queued after a failed/rejected dispatch had its sent-flag cleared,
    // so this re-offers it (workers score it a second time) - including to a
    // worker that just freed capacity via the kick. Sessions ignore an empty
    // delta, so this is cheap when nothing changed.
    let _ = scheduler.cast(SchedulerMsg::ReOffer).await;
}

pub(crate) enum BuildMsg {
    Tick,
    Kick,
}

/// The build dispatcher as a supervised actor: a timer tick every
/// `ASSIGN_TICK`, plus edge-triggered kicks coalesced by generation so a
/// burst of kicks queued during one pass is serviced by a single extra pass.
pub(crate) struct BuildAssigner;

pub(crate) struct BuildAssignerState {
    scheduler: Arc<Scheduler>,
    ctx: ChildCtx,
    kicks_seen: u64,
    resynced_at: Option<Instant>,
}

impl Actor for BuildAssigner {
    type Msg = BuildMsg;
    type State = BuildAssignerState;
    type Arguments = (Arc<Scheduler>, ChildCtx);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (scheduler, ctx): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        myself.send_after(ASSIGN_TICK, || BuildMsg::Tick);
        Ok(BuildAssignerState {
            scheduler,
            ctx,
            kicks_seen: 0,
            resynced_at: None,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let timer_tick = matches!(msg, BuildMsg::Tick);
        let kick_gen = state.scheduler.kick_gen.load(Ordering::Relaxed);
        if !timer_tick && kick_gen == state.kicks_seen {
            return Ok(());
        }

        let resync = timer_tick
            && state
                .resynced_at
                .is_none_or(|at| at.elapsed() >= STARTABLE_RESYNC);
        let scheduler = Arc::clone(&state.scheduler);
        let alive = run_pass(
            "build-dispatch",
            ASSIGN_BUDGET,
            &state.ctx.cancel,
            &state.ctx.health,
            Box::pin(async move {
                build_assign_pass(&scheduler, timer_tick, resync).await;
                Ok(())
            }),
        )
        .await;
        state.kicks_seen = kick_gen;
        if resync {
            state.resynced_at = Some(Instant::now());
        }

        if !alive {
            myself.stop(Some("shutdown".into()));
        } else if timer_tick {
            myself.send_after(ASSIGN_TICK, || BuildMsg::Tick);
        }
        Ok(())
    }
}

/// The build dispatcher as a supervised child. The factory re-publishes the
/// actor ref on every (re)spawn so `kick_assigner` always reaches the live one.
pub(super) fn child_spec(scheduler: &Arc<Scheduler>) -> ChildSpec {
    let scheduler = Arc::clone(scheduler);
    ChildSpec::Custom {
        stop_last: false,
        name: "build-dispatch",
        spawn: Arc::new(move |ctx: ChildCtx| {
            let scheduler = Arc::clone(&scheduler);
            Box::pin(async move {
                let parent = ctx.parent.clone();
                let (actor, _) =
                    Actor::spawn_linked(None, BuildAssigner, (Arc::clone(&scheduler), ctx), parent)
                        .await?;
                scheduler
                    .build_assigner
                    .store(Some(Arc::new(actor.clone())));
                Ok(actor.get_cell())
            })
        }),
    }
}

/// All DB data needed to assemble [`PendingBuildJob`]s for a dispatch pass.
///
/// Loaded in bulk (one IN-list query per table) by [`BuildAssignMaps::load`],
/// then queried in-memory by [`BuildAssignMaps::classify_assignment`].
/// This avoids O(n) serial round-trips when hundreds of builds become startable
/// at once.
struct BuildAssignMaps {
    derivations: HashMap<DerivationId, MDerivation>,
    evaluations: HashMap<EvaluationId, MEvaluation>,
    /// task_id -> project_id
    tasks: HashMap<TaskId, ProjectId>,
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_names: HashMap<FeatureId, String>,
    /// derivation_id -> number of direct dependencies
    dep_counts: HashMap<DerivationId, u32>,
    /// derivation_id -> direct input store paths (outputs of every input
    /// derivation). Used by workers to score how much they would have to
    /// download to start this build. `inputSrcs` are not included - they
    /// live in the `.drv` file and are not stored in the scheduler DB.
    direct_inputs: HashMap<DerivationId, Vec<RequiredPath>>,
    /// derivation_id -> this derivation's own output `(name, store_path)` pairs,
    /// sent on every `BuildSpec` so a Substitute fetches the outputs without
    /// fetching the `.drv`.
    self_outputs: HashMap<DerivationId, Vec<DerivationOutput>>,
    /// derivation_id -> transitive closure size (bytes), from
    /// `derivation.closure_size` or computed here when it is NULL.
    closure_sizes: HashMap<DerivationId, Option<i64>>,
    /// The sizes this pass computed; the graph writer persists them.
    computed_sizes: HashMap<DerivationId, i64>,
    /// derivation_id -> historical resource prediction by `(history_name,
    /// architecture)`, default when there is no matching history.
    histories: HashMap<DerivationId, gradient_pool::score::HistoryPrediction>,
    /// derivation_build -> the evaluation driving this shared build's dispatch (used for
    /// peer routing and `build_job` attribution on win). Prefers a non-terminal eval.
    driving_eval: HashMap<DerivationBuildId, EvaluationId>,
    /// Shared builds named by a live prioritized evaluation, on top of the shared builds
    /// flagged themselves.
    prioritized_by_eval: HashSet<DerivationBuildId>,
    config: AssignConfig,
}

/// The scalar dispatch knobs, split from the per-pass lookup maps.
struct AssignConfig {
    default_timeout_secs: Option<u64>,
    default_max_silent_secs: Option<u64>,
}

impl AssignConfig {
    fn from_state(state: &ServerState) -> Self {
        Self {
            default_timeout_secs: nonzero(state.config.build.default_timeout_secs),
            default_max_silent_secs: nonzero(state.config.build.default_max_silent_secs),
        }
    }
}

impl BuildAssignMaps {
    /// Issue one IN-list query per table and build all lookup maps.
    async fn load(
        state: &Arc<ServerState>,
        shared_builds: &[MDerivationBuild],
        uses_history: bool,
    ) -> anyhow::Result<Self> {
        // Every load below propagates its error: a failed query must abort the
        // dispatch pass (retried next tick) instead of masquerading as "no
        // rows", which dispatched builds against phantom-empty inputs.
        let drv_ids: Vec<DerivationId> = shared_builds.iter().map(|a| a.derivation).collect();

        let db = &state.worker_db;

        // Resolve the eval driving each shared build's dispatch: any referencing
        // build_job, preferring one whose evaluation is not terminal. The driving
        // eval is the job's peer-routing source and the build_job attributed on win.
        let mut driving_eval: HashMap<DerivationBuildId, EvaluationId> = HashMap::new();
        let jobs_by_drv = gradient_db::build_jobs_for_derivations(db, &drv_ids).await?;
        let mut jobs_by_shared_build: HashMap<DerivationBuildId, Vec<EvaluationId>> =
            HashMap::new();
        for shared_build in shared_builds {
            let evals = jobs_by_drv
                .get(&shared_build.derivation)
                .map(|jobs| jobs.iter().map(|j| j.evaluation).collect())
                .unwrap_or_default();
            jobs_by_shared_build.insert(shared_build.id, evals);
        }

        let eval_ids: Vec<EvaluationId> = jobs_by_shared_build
            .values()
            .flatten()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let derivations: HashMap<DerivationId, MDerivation> =
            gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
                EDerivation::find()
                    .filter(CDerivation::Id.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect();

        let evaluations: HashMap<EvaluationId, MEvaluation> =
            gradient_db::fetch_in_chunks(&eval_ids, |chunk| async move {
                EEvaluation::find()
                    .filter(CEvaluation::Id.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|e| (e.id, e))
            .collect();

        // Pick the driving eval per shared build: prefer one whose evaluation is not
        // terminal so dispatch attributes the build to a live eval.
        for (shared_build_id, eval_list) in &jobs_by_shared_build {
            let chosen = eval_list
                .iter()
                .find(|e| {
                    evaluations
                        .get(*e)
                        .is_some_and(|ev| !eval_is_terminal(ev.status))
                })
                .or_else(|| eval_list.first());
            if let Some(e) = chosen {
                driving_eval.insert(*shared_build_id, *e);
            }
        }

        let prioritized_by_eval = prioritized_by_eval(&jobs_by_shared_build, &evaluations);

        // project_id resolution: every evaluation must belong to a task.
        let task_ids: Vec<TaskId> = evaluations
            .values()
            .filter_map(|e| e.task)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let tasks: HashMap<TaskId, ProjectId> =
            gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
                ETask::find().filter(CTask::Id.is_in(chunk)).all(db).await
            })
            .await?
            .into_iter()
            .map(|p| (p.id, p.project))
            .collect();

        // Required features: per-derivation list of feature names.
        let feature_edges = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationFeature::find()
                .filter(CDerivationFeature::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?;
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        for e in &feature_edges {
            features_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.feature);
        }
        let feature_names: HashMap<FeatureId, String> = if feature_edges.is_empty() {
            HashMap::new()
        } else {
            let feature_ids: Vec<FeatureId> = feature_edges.iter().map(|e| e.feature).collect();
            gradient_db::fetch_in_chunks(&feature_ids, |chunk| async move {
                EFeature::find()
                    .filter(CFeature::Id.is_in(chunk))
                    .filter(CFeature::Kind.eq(gradient_entity::feature::FeatureKind::Feature))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|f| (f.id, f.name))
            .collect()
        };

        // Direct dependency edges per derivation. Used both for the scoring
        // policy's `dep_counts` and to build `direct_inputs` below.
        let dep_edges = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationDependency::find()
                .filter(CDerivationDependency::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?;

        let mut deps_by_drv: HashMap<DerivationId, Vec<DerivationId>> = HashMap::new();
        for e in &dep_edges {
            deps_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.dependency);
        }
        let dep_counts: HashMap<DerivationId, u32> = deps_by_drv
            .iter()
            .map(|(k, v)| (*k, v.len() as u32))
            .collect();

        // Direct input store paths per build derivation: for each input drv,
        // gather its `derivation_output` rows; for each output, attach
        // cache info from `cached_path` when available.
        let dep_drv_ids: Vec<DerivationId> = dep_edges
            .iter()
            .map(|e| e.dependency)
            .collect::<HashSet<DerivationId>>()
            .into_iter()
            .collect();

        let outputs_by_drv: HashMap<DerivationId, Vec<MDerivationOutput>> =
            if dep_drv_ids.is_empty() {
                HashMap::new()
            } else {
                let outs = gradient_db::fetch_in_chunks(&dep_drv_ids, |chunk| async move {
                    EDerivationOutput::find()
                        .filter(CDerivationOutput::Derivation.is_in(chunk))
                        .all(db)
                        .await
                })
                .await?;
                let mut map: HashMap<DerivationId, Vec<MDerivationOutput>> = HashMap::new();
                for o in outs {
                    map.entry(o.derivation).or_default().push(o);
                }
                map
            };

        let output_hashes: Vec<String> = outputs_by_drv
            .values()
            .flat_map(|v| v.iter().map(|o| o.hash.clone()))
            .collect::<HashSet<String>>()
            .into_iter()
            .collect();

        let cache_info_by_hash: HashMap<String, CacheInfo> =
            gradient_db::fetch_in_chunks(&output_hashes, |chunk| async move {
                ECachedPath::find()
                    .filter(CCachedPath::Hash.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .filter_map(|cp| {
                let nar_size = cp.nar_size? as u64;
                let file_size = cp.file_size.unwrap_or(0) as u64;
                Some((
                    cp.hash,
                    CacheInfo {
                        file_size,
                        nar_size,
                    },
                ))
            })
            .collect();

        let mut direct_inputs: HashMap<DerivationId, Vec<RequiredPath>> = HashMap::new();
        for (drv_id, dep_drvs) in &deps_by_drv {
            let mut paths: Vec<RequiredPath> = Vec::new();
            for dep_id in dep_drvs {
                let Some(outputs) = outputs_by_drv.get(dep_id) else {
                    continue;
                };
                for o in outputs {
                    let cache_info = cache_info_by_hash.get(&o.hash).cloned();
                    paths.push(RequiredPath {
                        path: get_path_from_derivation_output(o.clone()).full(),
                        cache_info,
                    });
                }
            }
            direct_inputs.insert(*drv_id, paths);
        }

        // This derivation's own outputs, on every BuildSpec: a Substitute fetches
        // exactly these and never the `.drv`.
        let mut self_outputs: HashMap<DerivationId, Vec<DerivationOutput>> = HashMap::new();
        for o in gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        {
            let path = get_path_from_derivation_output(o.clone()).full();
            self_outputs
                .entry(o.derivation)
                .or_default()
                .push(DerivationOutput { name: o.name, path });
        }

        let (closure_sizes, histories, computed_sizes) =
            load_sizes_and_histories(state, &derivations, uses_history).await;

        Ok(Self {
            derivations,
            evaluations,
            tasks,
            features_by_drv,
            feature_names,
            dep_counts,
            direct_inputs,
            self_outputs,
            closure_sizes,
            computed_sizes,
            histories,
            driving_eval,
            prioritized_by_eval,
            config: AssignConfig::from_state(state),
        })
    }

    /// Resolve the project that owns this evaluation (used as `project_id`
    /// to route the job only to workers registered by that project).
    fn resolve_project_id(&self, eval: &MEvaluation) -> Option<ProjectId> {
        eval.task.and_then(|pid| self.tasks.get(&pid).copied())
    }

    /// Return the required Nix system features for `derivation_id`.
    fn required_features(&self, derivation_id: DerivationId) -> Vec<String> {
        self.features_by_drv
            .get(&derivation_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| self.feature_names.get(i).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Decide whether `shared_build` dispatches this pass, and how - an explicit
    /// three-way outcome instead of a `None` that conflated "hard error" with
    /// "deliberately deferred".
    fn classify_assignment(&self, shared_build: &MDerivationBuild) -> AssignOutcome {
        let Some(derivation) = self.derivations.get(&shared_build.derivation) else {
            return AssignOutcome::Skip("derivation not found for shared build");
        };
        let Some(eval_id) = self.driving_eval.get(&shared_build.id).copied() else {
            return AssignOutcome::Skip("no driving evaluation for shared build");
        };
        let Some(eval) = self.evaluations.get(&eval_id) else {
            return AssignOutcome::Skip("driving evaluation row not found");
        };
        let Some(project_id) = self.resolve_project_id(eval) else {
            return AssignOutcome::Skip("could not resolve project_id for shared build");
        };

        let kind = decide_build_spec_kind(
            shared_build.cache_available,
            &derivation.architecture,
            derivation.is_fixed_output,
        );
        let (job_id, pending) =
            self.assemble_job(shared_build, derivation, eval_id, project_id, kind);
        AssignOutcome::Assign(job_id, Box::new(pending))
    }

    /// Pure assembly of the pending job once `classify_assignment` decided to go.
    fn assemble_job(
        &self,
        shared_build: &MDerivationBuild,
        derivation: &MDerivation,
        eval_id: EvaluationId,
        project_id: ProjectId,
        kind: BuildSpecKind,
    ) -> (String, PendingBuildJob) {
        let job_id = crate::jobs::build_job_key(shared_build.id);
        let substitute = kind == BuildSpecKind::Substitute;
        // Neither a Substitute nor a Download needs a nix store, so neither needs a
        // worker of the derivation's architecture, its features, or its inputs.
        let anywhere = kind != BuildSpecKind::Build;
        // The worker round-trips this shared build uuid as the opaque BuildSpec.build_id.
        let build_job = BuildJob {
            builds: vec![BuildSpec {
                build_id: shared_build.id.to_string(),
                drv_path: derivation.store_path(),
                kind,
                is_fixed_output: derivation.is_fixed_output,
                outputs: self
                    .self_outputs
                    .get(&shared_build.derivation)
                    .cloned()
                    .unwrap_or_default(),
                timeout_secs: resolve_limit(
                    shared_build.timeout_secs,
                    self.config.default_timeout_secs,
                ),
                max_silent_secs: resolve_limit(
                    shared_build.max_silent_secs,
                    self.config.default_max_silent_secs,
                ),
            }],
        };
        let (architecture, required_features) = if anywhere {
            (gradient_types::BUILTIN_ARCH.to_string(), Vec::new())
        } else {
            (
                derivation.architecture.clone(),
                self.required_features(shared_build.derivation),
            )
        };

        // A substitute or a download produces its outputs without the local store,
        // so it neither prefetches build-dependency inputs nor is worth scoring:
        // leaving required_paths empty stops the worker pulling deps and makes every
        // worker's score the same assumed zero (#456).
        let required_paths = if anywhere {
            Vec::new()
        } else {
            self.direct_inputs
                .get(&shared_build.derivation)
                .cloned()
                .unwrap_or_default()
        };

        let prioritized =
            shared_build.prioritized || self.prioritized_by_eval.contains(&shared_build.id);
        let pending = PendingBuildJob {
            derivation_build: shared_build.id,
            derivation: shared_build.derivation,
            evaluation_id: eval_id,
            project_id,
            job: build_job,
            required_paths,
            architecture,
            required_features,
            dependency_count: self
                .dep_counts
                .get(&shared_build.derivation)
                .copied()
                .unwrap_or(0),
            closure_size: self
                .closure_sizes
                .get(&shared_build.derivation)
                .copied()
                .flatten(),
            prefer_local_build: derivation.prefer_local_build,
            is_fixed_output: derivation.is_fixed_output,
            history: self
                .histories
                .get(&shared_build.derivation)
                .copied()
                .unwrap_or_default(),
            queued_at: shared_build.updated_at,
            ready_at: now(),
            rescore_count: 0,
            prioritized,
            pname: derivation.pname.clone(),
            substitute,
        };

        (job_id, pending)
    }
}

/// The dispatch decision for one ready shared build. `Queued` already means the gates
/// held, so there is no third "deliberately held this pass" outcome: a shared build that
/// should not run yet is not in the queue.
enum AssignOutcome {
    /// Enqueue this job now.
    Assign(String, Box<PendingBuildJob>),
    /// A required lookup failed; the shared build cannot be assembled.
    Skip(&'static str),
}

/// Closure sizes and historical predictions, consumed only by policies that
/// read history (resource-aware); the simple policy skips the walk and uses
/// whatever is persisted. Derivations missing `closure_size` are sized in one
/// batched walk, returned as the third element for the graph writer to persist;
/// history is queried once per distinct `(history_name, architecture)`.
async fn load_sizes_and_histories(
    state: &Arc<ServerState>,
    derivations: &HashMap<DerivationId, MDerivation>,
    uses_history: bool,
) -> (
    HashMap<DerivationId, Option<i64>>,
    HashMap<DerivationId, gradient_pool::score::HistoryPrediction>,
    HashMap<DerivationId, i64>,
) {
    let mut closure_sizes: HashMap<DerivationId, Option<i64>> = HashMap::new();
    let mut histories: HashMap<DerivationId, gradient_pool::score::HistoryPrediction> =
        HashMap::new();
    if !uses_history {
        for (drv_id, drv) in derivations {
            closure_sizes.insert(*drv_id, drv.closure_size);
        }
        return (closure_sizes, histories, HashMap::new());
    }

    let need: Vec<DerivationId> = derivations
        .iter()
        .filter(|(_, d)| d.closure_size.is_none())
        .map(|(id, _)| *id)
        .collect();
    let computed = if need.is_empty() {
        HashMap::new()
    } else {
        gradient_db::transitive_closure_sizes(&state.worker_db, &need)
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to compute closure sizes");
                HashMap::new()
            })
    };
    let mut predictions: HashMap<(&str, &str), gradient_pool::score::HistoryPrediction> =
        HashMap::new();
    for (drv_id, drv) in derivations {
        let size = drv.closure_size.or_else(|| computed.get(drv_id).copied());
        closure_sizes.insert(*drv_id, size);
        let pname = drv.history_name();
        let key = (pname, drv.architecture.as_str());
        let prediction = match predictions.get(&key) {
            Some(p) => *p,
            None => {
                let p = crate::history::predict(&state.worker_db, pname, &drv.architecture).await;
                predictions.insert(key, p);
                p
            }
        };
        histories.insert(*drv_id, prediction);
    }

    (closure_sizes, histories, computed)
}

fn prioritized_by_eval(
    jobs_by_shared_build: &HashMap<DerivationBuildId, Vec<EvaluationId>>,
    evaluations: &HashMap<EvaluationId, MEvaluation>,
) -> HashSet<DerivationBuildId> {
    jobs_by_shared_build
        .iter()
        .filter(|(_, evals)| {
            evals.iter().any(|e| {
                evaluations
                    .get(e)
                    .is_some_and(|ev| ev.prioritized && !eval_is_terminal(ev.status))
            })
        })
        .map(|(shared_build, _)| *shared_build)
        .collect()
}

/// Whether an evaluation has reached a terminal status (won't drive new builds).
fn eval_is_terminal(status: EvaluationStatus) -> bool {
    matches!(
        status,
        EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
    )
}

/// Admit the shared builds that entered `Queued` since the last pass and drop the
/// pending jobs of those that left it, so a pass costs what moved. An entered
/// shared build already pending is assembled afresh: its passthrough mode may have changed.
pub(crate) async fn admit_startable_moves(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.load(Ordering::Relaxed) {
        return Ok(());
    }

    let StartableMoves { entered, left } = scheduler.state.startable_set.take();
    if entered.is_empty() && left.is_empty() {
        return Ok(());
    }

    let derivations: Vec<DerivationId> = entered.iter().copied().collect();
    scheduler
        .prune_pending_builds(move |b| {
            left.contains(&b.derivation) || entered.contains(&b.derivation)
        })
        .await;
    let shared_builds =
        gradient_db::find_startable_shared_builds_among(&scheduler.state.worker_db, &derivations)
            .await?;

    enqueue_startable_shared_builds(scheduler, shared_builds).await
}

/// The whole startable set, at startup and every [`STARTABLE_RESYNC`]: it admits what no
/// move announced (a lost hint, another instance's promotion) and prunes the
/// pending builds that stopped being dispatchable without a move this instance
/// saw, such as one another instance claimed.
pub(crate) async fn resync_startable_set(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.load(Ordering::Relaxed) {
        return Ok(());
    }

    let shared_builds =
        gradient_db::find_startable_shared_builds(&scheduler.state.worker_db).await?;
    let startable: HashSet<DerivationBuildId> = shared_builds.iter().map(|a| a.id).collect();
    let pruned = scheduler
        .prune_pending_builds(move |b| !startable.contains(&b.derivation_build))
        .await;
    if pruned > 0 {
        debug!(
            pruned,
            "resync dropped pending builds no longer startable here"
        );
    }

    enqueue_startable_shared_builds(scheduler, shared_builds).await
}

/// Assemble and enqueue the shared builds the tracker does not hold yet. The select
/// re-derives no can-start term: it trusts `Queued` to mean the gates held,
/// which holds because of the one rule every writer of that status obeys, stated
/// at `graph_sql::promotable_predicate`.
async fn enqueue_startable_shared_builds(
    scheduler: &Scheduler,
    shared_builds: Vec<MDerivationBuild>,
) -> anyhow::Result<()> {
    if shared_builds.is_empty() {
        return Ok(());
    }

    let state = &scheduler.state;
    let started = std::time::Instant::now();
    let ids: Vec<String> = shared_builds
        .iter()
        .map(|a| crate::jobs::build_job_key(a.id))
        .collect();
    let untracked: HashSet<String> = scheduler.untracked(ids).await.into_iter().collect();
    let new_shared_builds: Vec<MDerivationBuild> = shared_builds
        .into_iter()
        .filter(|a| untracked.contains(&crate::jobs::build_job_key(a.id)))
        .collect();
    if new_shared_builds.is_empty() {
        return Ok(());
    }

    let enqueued_ids: Vec<_> = new_shared_builds.iter().map(|a| a.id).collect();
    let membership = crate::cluster::Membership::load(&state.worker_db, &[], &enqueued_ids).await?;

    let maps =
        BuildAssignMaps::load(state, &new_shared_builds, scheduler.policy.uses_history()).await?;

    let mut enqueued = 0usize;
    for shared_build in new_shared_builds {
        match maps.classify_assignment(&shared_build) {
            AssignOutcome::Assign(job_id, pending) => {
                let route = membership.route(&job_id);
                if let Err(e) = scheduler
                    .enqueue_routed(route, job_id, crate::jobs::PendingJob::Build(*pending))
                    .await
                {
                    warn!(error = %e, "enqueue_build_job failed");
                    continue;
                }
                enqueued += 1;
            }
            AssignOutcome::Skip(reason) => {
                error!(derivation_build = %shared_build.id, reason, "dispatch skipped");
            }
        }
    }

    // One write for the pass: the `ready_at` stamp on every shared build it enqueued and
    // the closure sizes it had to compute.
    if let Err(e) = state
        .graph
        .transition(Transition::Ready {
            shared_builds: enqueued_ids,
            closure_sizes: maps.computed_sizes.iter().map(|(k, v)| (*k, *v)).collect(),
        })
        .await
    {
        error!(error = %e, "ready_at stamp did not reach the graph writer");
    }

    debug!(
        enqueued,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "startable shared builds enqueued"
    );

    Ok(())
}

fn nonzero(v: u64) -> Option<u64> {
    (v != 0).then_some(v)
}

/// Per-derivation limit takes precedence over the server default. A stored `0` means "no limit".
fn resolve_limit(stored: Option<i64>, default: Option<u64>) -> Option<u64> {
    match stored {
        Some(0) => None,
        Some(v) if v > 0 => Some(v as u64),
        _ => default,
    }
}

#[cfg(test)]
mod limit_tests {
    use super::{nonzero, resolve_limit};

    #[test]
    fn per_drv_overrides_default() {
        assert_eq!(resolve_limit(Some(120), Some(3600)), Some(120));
    }

    #[test]
    fn zero_means_no_limit() {
        assert_eq!(resolve_limit(Some(0), Some(3600)), None);
        assert_eq!(nonzero(0), None);
    }

    #[test]
    fn falls_back_to_default_when_absent() {
        assert_eq!(resolve_limit(None, Some(3600)), Some(3600));
        assert_eq!(resolve_limit(None, None), None);
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;

    fn eval(status: EvaluationStatus, prioritized: bool) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status,
            prioritized,
            ..Default::default()
        }
    }

    #[test]
    fn only_a_live_prioritized_evaluation_lifts_the_shared_builds_it_names() {
        let live = eval(EvaluationStatus::Building, true);
        let finished = eval(EvaluationStatus::Completed, true);
        let plain = eval(EvaluationStatus::Building, false);
        let (shared, stale, unflagged) = (
            DerivationBuildId::now_v7(),
            DerivationBuildId::now_v7(),
            DerivationBuildId::now_v7(),
        );
        let jobs_by_shared_build = HashMap::from([
            (shared, vec![plain.id, live.id]),
            (stale, vec![finished.id]),
            (unflagged, vec![plain.id]),
        ]);
        let evaluations =
            HashMap::from([(live.id, live), (finished.id, finished), (plain.id, plain)]);

        assert_eq!(
            prioritized_by_eval(&jobs_by_shared_build, &evaluations),
            HashSet::from([shared])
        );
    }
}
