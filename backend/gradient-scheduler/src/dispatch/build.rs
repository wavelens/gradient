/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::dispatch_mode::{BuildDispatchMode, arch_available, decide_dispatch_mode};
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_sources::get_path_from_derivation_output;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use tracing::{debug, error, info};

use crate::Scheduler;
use crate::jobs::PendingBuildJob;
use gradient_types::proto::{BuildJob, BuildTask, CacheInfo, DerivationOutput, RequiredPath};

use super::DISPATCH_TICK_SECS;

/// Full-backstop (Global) reconcile cadence, in dispatch ticks: 6 x 5s = 30s.
const GLOBAL_RECONCILE_TICKS: u64 = 6;

/// Deep reconcile cadence (Global plus the cached_path closure re-derivation,
/// tens of seconds on a large cache): 720 x 5s = 1h.
const DEEP_RECONCILE_TICKS: u64 = 720;

pub(super) async fn build_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(DISPATCH_TICK_SECS));
    let cancel = scheduler.state.shutdown.token();
    let mut tick_count: u64 = 0;
    info!("build dispatch loop started");
    loop {
        let timer_tick = tokio::select! {
            _ = cancel.cancelled() => {
                info!("build dispatch loop shutting down");
                return;
            }
            _ = interval.tick() => true,
            _ = scheduler.dispatch_kick.notified() => false,
        };
        // rescore_count is an anti-starvation timeout measured in dispatch
        // intervals, so only the timer advances it - reactive kicks (which can
        // fire many times per interval) just run an extra dispatch pass.
        if timer_tick {
            scheduler.job_tracker.write().await.bump_rescore_counts();

            // Every timer tick runs promotion (Tick); the anchor-side flag
            // fixpoints, unbacked-output demote, and failure sweep (Global) run
            // every GLOBAL_RECONCILE_TICKS, and the cached_path-side fixpoint
            // (Deep) hourly - their full-table scans saturated Postgres when
            // re-run every 5s on a large graph. Active evals keep their flags
            // fresh via reactive completion propagation and per-flush Eval passes.
            tick_count = tick_count.wrapping_add(1);
            let scope = if tick_count.is_multiple_of(DEEP_RECONCILE_TICKS) {
                gradient_db::ReconcileScope::Deep
            } else if tick_count.is_multiple_of(GLOBAL_RECONCILE_TICKS) {
                gradient_db::ReconcileScope::Global
            } else {
                gradient_db::ReconcileScope::Tick
            };
            gradient_db::reconcile_build_graph(&scheduler.state.db(), scope).await;
        }

        if let Err(e) = requeue_transient_failures(&scheduler).await {
            error!(error = %e, "requeue_transient_failures error");
        }
        if let Err(e) = dispatch_ready_builds(&scheduler).await {
            error!(error = %e, "build dispatch error");
        }
        // After dispatching, reconcile each in-flight evaluation's
        // Building/Waiting state so the UI reflects "no worker can pick this
        // up" (or recovers when a worker comes back online). Cheap when
        // there are no in-flight evals.
        if let Err(e) = scheduler.reconcile_waiting_state().await {
            error!(error = %e, "reconcile_waiting_state in dispatch loop failed");
        }

        // Re-offer still-pending jobs to all sessions each pass. A build
        // re-queued after a failed/rejected dispatch had its sent-flag cleared,
        // so this re-offers it (workers score it a second time) - including to a
        // worker that just freed capacity via the kick. Sessions ignore an empty
        // delta, so this is cheap when nothing changed.
        if scheduler.job_tracker.read().await.has_pending() {
            scheduler.job_notify.send_modify(|g| *g = g.wrapping_add(1));
        }
    }
}

/// Move `FailedTransient` anchors whose backoff window has elapsed back to
/// `Queued` so the ready-builds pass can dispatch them again.
pub(crate) async fn requeue_transient_failures(scheduler: &Scheduler) -> anyhow::Result<()> {
    use crate::build::retry_backoff_elapsed;
    let state = &scheduler.state;
    let base = state.config.eval.build_retry_backoff_secs;
    let now = gradient_types::now();

    let transient = EDerivationBuild::find()
        .filter(CDerivationBuild::Status.eq(BuildStatus::FailedTransient))
        .all(&state.worker_db)
        .await?;
    for anchor in transient {
        if retry_backoff_elapsed(anchor.attempt, anchor.updated_at, now, base) {
            gradient_db::update_derivation_build_status(&state.db(), anchor, BuildStatus::Queued)
                .await;
        }
    }
    Ok(())
}

/// All DB data needed to assemble [`PendingBuildJob`]s for a dispatch pass.
///
/// Loaded in bulk (one IN-list query per table) by [`BuildDispatchMaps::load`],
/// then queried in-memory by [`BuildDispatchMaps::classify_dispatch`].
/// This avoids O(n) serial round-trips when hundreds of builds become ready
/// at once.
struct BuildDispatchMaps {
    derivations: HashMap<DerivationId, MDerivation>,
    evaluations: HashMap<EvaluationId, MEvaluation>,
    /// project_id → organization_id
    projects: HashMap<ProjectId, OrganizationId>,
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_names: HashMap<FeatureId, String>,
    /// derivation_id → number of direct dependencies
    dep_counts: HashMap<DerivationId, u32>,
    /// derivation_id → direct input store paths (outputs of every input
    /// derivation). Used by workers to score how much they would have to
    /// download to start this build. `inputSrcs` are not included - they
    /// live in the `.drv` file and are not stored in the scheduler DB.
    direct_inputs: HashMap<DerivationId, Vec<RequiredPath>>,
    /// derivation_id → this derivation's own output `(name, store_path)` pairs,
    /// sent in the `external_cached` `BuildTask` so the worker substitutes the
    /// outputs without fetching the `.drv`.
    self_outputs: HashMap<DerivationId, Vec<DerivationOutput>>,
    /// derivation_id → transitive closure size (bytes). Loaded from
    /// `derivation.closure_size`; NULLs are computed once and persisted here.
    closure_sizes: HashMap<DerivationId, Option<i64>>,
    /// derivation_id → historical resource prediction (default when the
    /// derivation has no `pname` or no matching history).
    histories: HashMap<DerivationId, gradient_score::HistoryPrediction>,
    /// (derivation_build, driving evaluation) → `SubstituteUnavailable` miss count,
    /// scoped to that evaluation so a new eval retries substitution from zero.
    /// Absent ⇒ 0.
    substitute_misses: HashMap<(DerivationBuildId, EvaluationId), i64>,
    /// derivation_build → the evaluation driving this anchor's dispatch (used for
    /// peer routing and `build_job` attribution on win). Prefers a non-terminal eval.
    driving_eval: HashMap<DerivationBuildId, EvaluationId>,
    connected_architectures: HashSet<String>,
    config: DispatchConfig,
}

/// The scalar dispatch knobs, split from the per-pass lookup maps.
struct DispatchConfig {
    substitute_miss_escalation_threshold: i64,
    build_retry_backoff_secs: u64,
    default_timeout_secs: Option<u64>,
    default_max_silent_secs: Option<u64>,
}

impl DispatchConfig {
    fn from_state(state: &ServerState) -> Self {
        Self {
            substitute_miss_escalation_threshold: state
                .config
                .eval
                .substitute_miss_escalation_threshold
                as i64,
            build_retry_backoff_secs: state.config.eval.build_retry_backoff_secs,
            default_timeout_secs: nonzero(state.config.eval.build_default_timeout_secs),
            default_max_silent_secs: nonzero(state.config.eval.build_default_max_silent_secs),
        }
    }
}

impl BuildDispatchMaps {
    /// Issue one IN-list query per table and build all lookup maps.
    async fn load(
        state: &Arc<ServerState>,
        anchors: &[MDerivationBuild],
        uses_history: bool,
        connected_architectures: HashSet<String>,
    ) -> anyhow::Result<Self> {
        // Every load below propagates its error: a failed query must abort the
        // dispatch pass (retried next tick) instead of masquerading as "no
        // rows", which dispatched builds against phantom-empty inputs.
        let drv_ids: Vec<DerivationId> = anchors.iter().map(|a| a.derivation).collect();
        let anchor_ids: Vec<DerivationBuildId> = anchors.iter().map(|a| a.id).collect();

        let db = &state.worker_db;
        let substitute_misses = gradient_db::substitute_miss_counts(db, &anchor_ids).await?;

        // Resolve the eval driving each anchor's dispatch: any referencing
        // build_job, preferring one whose evaluation is not terminal. The driving
        // eval is the job's peer-routing source and the build_job attributed on win.
        let mut driving_eval: HashMap<DerivationBuildId, EvaluationId> = HashMap::new();
        let jobs_by_drv = gradient_db::build_jobs_for_derivations(db, &drv_ids).await?;
        let mut jobs_by_anchor: HashMap<DerivationBuildId, Vec<EvaluationId>> = HashMap::new();
        for anchor in anchors {
            let evals = jobs_by_drv
                .get(&anchor.derivation)
                .map(|jobs| jobs.iter().map(|j| j.evaluation).collect())
                .unwrap_or_default();
            jobs_by_anchor.insert(anchor.id, evals);
        }

        let eval_ids: Vec<EvaluationId> = jobs_by_anchor
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

        // Pick the driving eval per anchor: prefer one whose evaluation is not
        // terminal so dispatch attributes the build to a live eval.
        for (anchor_id, eval_list) in &jobs_by_anchor {
            let chosen = eval_list
                .iter()
                .find(|e| {
                    evaluations
                        .get(*e)
                        .is_some_and(|ev| !eval_is_terminal(ev.status))
                })
                .or_else(|| eval_list.first());
            if let Some(e) = chosen {
                driving_eval.insert(*anchor_id, *e);
            }
        }

        // org_id resolution: every evaluation must belong to a project.
        let project_ids: Vec<ProjectId> = evaluations
            .values()
            .filter_map(|e| e.project)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let projects: HashMap<ProjectId, OrganizationId> =
            gradient_db::fetch_in_chunks(&project_ids, |chunk| async move {
                EProject::find()
                    .filter(CProject::Id.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|p| (p.id, p.organization))
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

        // This derivation's own outputs, for the external_cached BuildTask: the
        // worker substitutes these output paths directly without fetching the
        // .drv (whose build-time input_sources binary caches do not serve).
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

        let (closure_sizes, histories) =
            load_sizes_and_histories(state, &derivations, uses_history).await;

        Ok(Self {
            derivations,
            evaluations,
            projects,
            features_by_drv,
            feature_names,
            dep_counts,
            direct_inputs,
            self_outputs,
            closure_sizes,
            histories,
            substitute_misses,
            driving_eval,
            connected_architectures,
            config: DispatchConfig::from_state(state),
        })
    }

    /// Resolve the organization that owns this evaluation (used as `org_id`
    /// to route the job only to workers registered by that org).
    fn resolve_org_id(&self, eval: &MEvaluation) -> Option<OrganizationId> {
        eval.project
            .and_then(|pid| self.projects.get(&pid).copied())
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

    /// Decide whether `anchor` dispatches this pass, and how - an explicit
    /// three-way outcome instead of a `None` that conflated "hard error" with
    /// "deliberately deferred".
    fn classify_dispatch(&self, anchor: &MDerivationBuild) -> DispatchOutcome {
        let Some(derivation) = self.derivations.get(&anchor.derivation) else {
            return DispatchOutcome::Skip("derivation not found for anchor");
        };
        let Some(eval_id) = self.driving_eval.get(&anchor.id).copied() else {
            return DispatchOutcome::Skip("no driving evaluation for anchor");
        };
        let Some(eval) = self.evaluations.get(&eval_id) else {
            return DispatchOutcome::Skip("driving evaluation row not found");
        };
        let Some(org_id) = self.resolve_org_id(eval) else {
            return DispatchOutcome::Skip("could not resolve org_id for anchor");
        };

        let miss_count = self
            .substitute_misses
            .get(&(anchor.id, eval_id))
            .copied()
            .unwrap_or(0);
        let arch_has_worker =
            arch_available(&self.connected_architectures, &derivation.architecture);
        let mode = decide_dispatch_mode(
            anchor.substitutable,
            miss_count,
            self.config.substitute_miss_escalation_threshold,
            arch_has_worker,
        );

        if mode == BuildDispatchMode::SubstituteStalled
            && !crate::build::retry_backoff_elapsed(
                miss_count as i32,
                anchor.updated_at,
                now(),
                self.config.build_retry_backoff_secs,
            )
        {
            return DispatchOutcome::Defer(
                "substitute stalled (no arch worker); backing off re-probe",
            );
        }

        let (job_id, pending) = self.assemble_job(anchor, derivation, eval_id, org_id, mode);
        DispatchOutcome::Dispatch(job_id, Box::new(pending))
    }

    /// Pure assembly of the pending job once `classify_dispatch` decided to go.
    fn assemble_job(
        &self,
        anchor: &MDerivationBuild,
        derivation: &MDerivation,
        eval_id: EvaluationId,
        org_id: OrganizationId,
        mode: BuildDispatchMode,
    ) -> (String, PendingBuildJob) {
        let job_id = format!("build:{}", anchor.id);
        let substitute = matches!(
            mode,
            BuildDispatchMode::SubstituteBuiltin | BuildDispatchMode::SubstituteStalled
        );
        // The worker round-trips this anchor uuid as the opaque BuildTask.build_id.
        let outputs = if substitute {
            self.self_outputs
                .get(&anchor.derivation)
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let build_job = BuildJob {
            builds: vec![BuildTask {
                build_id: anchor.id.to_string(),
                drv_path: derivation.store_path(),
                external_cached: substitute,
                is_fixed_output: derivation.is_fixed_output,
                outputs,
                timeout_secs: resolve_limit(anchor.timeout_secs, self.config.default_timeout_secs),
                max_silent_secs: resolve_limit(
                    anchor.max_silent_secs,
                    self.config.default_max_silent_secs,
                ),
            }],
        };
        let (architecture, required_features) = if substitute {
            (gradient_types::BUILTIN_ARCH.to_string(), Vec::new())
        } else {
            (
                derivation.architecture.clone(),
                self.required_features(anchor.derivation),
            )
        };

        // A substitute job downloads its outputs straight from upstream, so it
        // neither prefetches build-dependency inputs nor is worth scoring: leaving
        // required_paths empty stops the worker pulling deps and makes every
        // worker's score the same assumed zero (#456).
        let required_paths = if substitute {
            Vec::new()
        } else {
            self.direct_inputs
                .get(&anchor.derivation)
                .cloned()
                .unwrap_or_default()
        };

        let pending = PendingBuildJob {
            derivation_build: anchor.id,
            evaluation_id: eval_id,
            org_id,
            job: build_job,
            required_paths,
            architecture,
            required_features,
            dependency_count: self
                .dep_counts
                .get(&anchor.derivation)
                .copied()
                .unwrap_or(0),
            closure_size: self
                .closure_sizes
                .get(&anchor.derivation)
                .copied()
                .flatten(),
            prefer_local_build: derivation.prefer_local_build,
            is_fixed_output: derivation.is_fixed_output,
            history: self
                .histories
                .get(&anchor.derivation)
                .copied()
                .unwrap_or_default(),
            queued_at: anchor.updated_at,
            ready_at: now(),
            rescore_count: 0,
            pname: derivation.pname.clone(),
            substitute,
        };

        (job_id, pending)
    }
}

/// The dispatch decision for one ready anchor.
enum DispatchOutcome {
    /// Enqueue this job now.
    Dispatch(String, Box<PendingBuildJob>),
    /// Deliberately held this pass (re-probed on a later tick).
    Defer(&'static str),
    /// A required lookup failed; the anchor cannot be assembled.
    Skip(&'static str),
}

/// Closure sizes and historical predictions, consumed only by policies that
/// read history (resource-aware); the simple policy skips the walk and uses
/// whatever is persisted. Derivations missing `closure_size` are sized in one
/// batched walk and backfilled; history is queried once per distinct
/// `(pname, size bucket)` instead of once per derivation.
async fn load_sizes_and_histories(
    state: &Arc<ServerState>,
    derivations: &HashMap<DerivationId, MDerivation>,
    uses_history: bool,
) -> (
    HashMap<DerivationId, Option<i64>>,
    HashMap<DerivationId, gradient_score::HistoryPrediction>,
) {
    let mut closure_sizes: HashMap<DerivationId, Option<i64>> = HashMap::new();
    let mut histories: HashMap<DerivationId, gradient_score::HistoryPrediction> = HashMap::new();
    if !uses_history {
        for (drv_id, drv) in derivations {
            closure_sizes.insert(*drv_id, drv.closure_size);
        }
        return (closure_sizes, histories);
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
    for (drv_id, size) in &computed {
        if let Err(e) = EDerivation::update_many()
            .col_expr(
                CDerivation::ClosureSize,
                sea_orm::sea_query::Expr::value(*size),
            )
            .filter(CDerivation::Id.eq(*drv_id))
            .exec(&state.worker_db)
            .await
        {
            error!(derivation_id = %drv_id, error = %e, "failed to persist closure size");
        }
    }

    let mut predictions: HashMap<(String, Option<i64>), gradient_score::HistoryPrediction> =
        HashMap::new();
    for (drv_id, drv) in derivations {
        let size = drv.closure_size.or_else(|| computed.get(drv_id).copied());
        closure_sizes.insert(*drv_id, size);
        let Some(pname) = &drv.pname else { continue };
        let key = (pname.clone(), size.map(crate::history::closure_bucket));
        let prediction = match predictions.get(&key) {
            Some(p) => *p,
            None => {
                let p = crate::history::predict(&state.worker_db, pname, size).await;
                predictions.insert(key, p);
                p
            }
        };
        histories.insert(*drv_id, prediction);
    }

    (closure_sizes, histories)
}

/// Whether an evaluation has reached a terminal status (won't drive new builds).
fn eval_is_terminal(status: EvaluationStatus) -> bool {
    matches!(
        status,
        EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
    )
}

pub(crate) async fn dispatch_ready_builds(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.load(Ordering::Relaxed) {
        return Ok(());
    }

    let state = &scheduler.state;

    // The dispatch gate lives in gradient_db next to promotion so both embed the
    // one shared readiness predicate; see `gradient_db::find_ready_anchors`.
    let started = std::time::Instant::now();
    let anchors = gradient_db::find_ready_anchors(&state.worker_db).await?;
    if anchors.is_empty() {
        return Ok(());
    }

    // Filter out anchors already in the in-memory tracker - one lock acquisition
    // for the whole pass instead of per-anchor.
    let new_anchors: Vec<MDerivationBuild> = {
        let tracker = scheduler.job_tracker.read().await;
        anchors
            .into_iter()
            .filter(|a| !tracker.contains_job(&format!("build:{}", a.id)))
            .collect()
    };
    if new_anchors.is_empty() {
        return Ok(());
    }

    // Stamp ready_at the first time an anchor becomes dispatchable (deps satisfied).
    let ready_ids: Vec<_> = new_anchors.iter().map(|a| a.id).collect();
    let db = &state.worker_db;
    if let Err(e) = gradient_db::for_each_chunk(&ready_ids, |chunk| async move {
        EDerivationBuild::update_many()
            .col_expr(
                CDerivationBuild::ReadyAt,
                sea_orm::sea_query::Expr::value(now()),
            )
            .filter(CDerivationBuild::Id.is_in(chunk))
            .filter(CDerivationBuild::ReadyAt.is_null())
            .exec(db)
            .await
    })
    .await
    {
        error!(error = %e, "failed to stamp anchor ready_at");
    }

    let connected_architectures: HashSet<String> = scheduler
        .worker_pool
        .read()
        .await
        .all_workers()
        .into_iter()
        .flat_map(|w| w.architectures)
        .collect();

    let maps = BuildDispatchMaps::load(
        state,
        &new_anchors,
        scheduler.policy.uses_history(),
        connected_architectures,
    )
    .await?;

    let mut enqueued = 0usize;
    for anchor in new_anchors {
        match maps.classify_dispatch(&anchor) {
            DispatchOutcome::Dispatch(job_id, pending) => {
                scheduler.enqueue_build_job(job_id, *pending).await;
                enqueued += 1;
            }
            DispatchOutcome::Defer(reason) => {
                debug!(derivation_build = %anchor.id, reason, "dispatch deferred");
            }
            DispatchOutcome::Skip(reason) => {
                error!(derivation_build = %anchor.id, reason, "dispatch skipped");
            }
        }
    }
    debug!(
        enqueued,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "dispatch_ready_builds completed"
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
mod dispatch_mode_tests {
    use crate::dispatch_mode::{BuildDispatchMode, decide_dispatch_mode};

    #[test]
    fn stalled_substitute_stays_builtin() {
        assert_eq!(
            decide_dispatch_mode(true, 2, 2, false),
            BuildDispatchMode::SubstituteStalled
        );
        assert_eq!(
            decide_dispatch_mode(true, 2, 2, true),
            BuildDispatchMode::RealArch
        );
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
