/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background loops that poll the DB and enqueue jobs into the in-memory scheduler.
//!
//! Three loops run concurrently:
//! - `trigger_dispatch::trigger_dispatch_loop`: fires polling/time triggers → creates evaluations
//! - `eval_dispatch_loop`: finds `Queued` evaluations → enqueues `FlakeJob`s
//! - `build_dispatch_loop`: finds ready `Queued` `derivation_build` anchors → enqueues `BuildJob`s
//!
//! The eval/build loops are idempotent: re-enqueueing the same job_id overwrites
//! the existing entry in the `JobTracker` without harm.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::dispatch_mode::{anchor_substitutable, arch_available, decide_dispatch_mode, BuildDispatchMode};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_sources::get_path_from_derivation_output;
use gradient_types::input::vec_to_hex;
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use tracing::{debug, error, info};

use super::Scheduler;
use super::jobs::{PendingBuildJob, PendingEvalJob};
use gradient_types::proto::{
    BuildJob, BuildTask, CacheInfo, FlakeJob, FlakeTask, RequiredPath,
};

/// Spawns all dispatch loops on the shared shutdown tracker so they drain on SIGTERM.
pub fn start_dispatch_loops(scheduler: Arc<Scheduler>) {
    let shutdown = scheduler.state.shutdown.clone();
    let s1 = Arc::clone(&scheduler);
    let s2 = Arc::clone(&scheduler);
    let s3 = Arc::clone(&scheduler);
    let s4 = Arc::clone(&scheduler);
    let s5 = Arc::clone(&scheduler);
    shutdown.spawn(async move { super::trigger_dispatch::trigger_dispatch_loop(s3).await });
    shutdown.spawn(async move { eval_dispatch_loop(s1).await });
    shutdown.spawn(async move { build_dispatch_loop(s2).await });
    shutdown.spawn(async move { worker_sample_loop(s4).await });
    shutdown.spawn(async move { instance_metrics_loop(s5).await });
}

/// Periodically recompute the windowed [`gradient_score::InstanceContext`] snapshot
/// consumed by resource-aware scoring and publish it lock-free.
async fn instance_metrics_loop(scheduler: Arc<Scheduler>) {
    let secs = scheduler
        .state
        .config
        .metrics_args
        .instance_metrics_interval_secs
        .max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        let (active_builds, pending_builds) =
            scheduler.job_tracker.read().await.instance_counts();
        let (total_workers, idle_workers) = scheduler.worker_pool.read().await.worker_counts();
        let counts = crate::instance::InstanceCounts {
            active_builds,
            pending_builds,
            total_workers,
            idle_workers,
        };
        let ctx = crate::instance::compute_instance_context(
            &scheduler.state.worker_db,
            counts,
            gradient_types::now(),
        )
        .await;
        scheduler.instance.store(Arc::new(ctx));

        let eval_history =
            crate::instance::compute_eval_history(&scheduler.state.worker_db, gradient_types::now()).await;
        scheduler.eval_history.store(Arc::new(eval_history));
    }
}

/// Periodically snapshot every connected worker's live metrics into
/// `worker_sample` for the Job Board's worker statistics.
async fn worker_sample_loop(scheduler: Arc<Scheduler>) {
    let secs = scheduler
        .state
        .config
        .metrics_args
        .worker_sample_interval_secs
        .max(1);
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        let workers = scheduler.worker_pool.read().await.all_workers();
        for info in &workers {
            super::worker_lifecycle::record_worker_sample(&scheduler.state.worker_db, info).await;
        }
        let (workers, pending, active) = scheduler.metrics_snapshot().await;
        let _ = scheduler
            .state
            .board_events
            .send(crate::BoardEvent::QueueDepth { workers, pending, active });
    }
}

// ── Eval dispatch ─────────────────────────────────────────────────────────────

async fn eval_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let cancel = scheduler.state.shutdown.token();
    info!("eval dispatch loop started");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("eval dispatch loop shutting down");
                return;
            }
            _ = interval.tick() => {}
        }
        if let Err(e) = dispatch_queued_evals(&scheduler).await {
            error!(error = %e, "eval dispatch error");
        }
    }
}

pub(crate) async fn dispatch_queued_evals(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.load(Ordering::Relaxed) {
        return Ok(());
    }

    let state = &scheduler.state;

    let evals = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Queued))
        .all(&state.worker_db)
        .await?;

    for eval in evals {
        let job_id = format!("eval:{}", eval.id);

        // Skip if already in the scheduler (pending or active).
        if scheduler.job_tracker.read().await.contains_job(&job_id) {
            continue;
        }

        let commit = match ECommit::find_by_id(eval.commit)
            .one(&state.worker_db)
            .await?
        {
            Some(c) => c,
            None => {
                error!(evaluation_id = %eval.id, "commit not found for evaluation");
                continue;
            }
        };

        let commit_sha = vec_to_hex(&commit.hash);

        let input_overrides = {
            use gradient_entity::evaluation_flake_input_override as efio;
            use sea_orm::QueryOrder;
            efio::Entity::find()
                .filter(efio::Column::Evaluation.eq(eval.id))
                .order_by_asc(efio::Column::InputName)
                .all(&state.worker_db)
                .await?
                .into_iter()
                .map(|r| gradient_types::proto::FlakeInputOverride {
                    input_name: r.input_name,
                    url: r.url,
                })
                .collect::<Vec<_>>()
        };

        let input_update = if eval.kind == gradient_entity::evaluation::EvaluationKind::InputUpdate
        {
            use gradient_entity::evaluation_input_update as eiu;
            eiu::Entity::find()
                .filter(eiu::Column::Evaluation.eq(eval.id))
                .one(&state.worker_db)
                .await?
                .map(|s| gradient_types::proto::InputUpdateSpec {
                    generator: s.generator,
                    inputs: s
                        .target_inputs
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
        } else {
            None
        };

        let split_fetch = scheduler.worker_pool.read().await.has_idle_eval_only_worker();
        let wildcards = eval
            .wildcard
            .parse::<Wildcard>()
            .map(|w| w.patterns().to_vec())
            .unwrap_or_else(|_| vec![eval.wildcard.clone()]);
        let (flake_job, required_paths) = flake_job_for_eval_source(
            &eval.repository,
            commit_sha,
            wildcards,
            split_fetch,
            input_overrides,
            input_update,
        );

        let organization_id = organization_id_for_eval(state, &eval).await;
        let org_id = match organization_id {
            Some(id) => id,
            None => {
                error!(evaluation_id = %eval.id, "could not determine organization for evaluation");
                continue;
            }
        };

        let history = eval
            .project
            .and_then(|p| scheduler.eval_history.load().get(&p).copied())
            .unwrap_or_default();

        let pending = PendingEvalJob {
            evaluation_id: eval.id,
            project_id: eval.project,
            peer_id: org_id,
            commit_id: eval.commit,
            repository: eval.repository.clone(),
            job: flake_job,
            required_paths,
            queued_at: eval.updated_at,
            ready_at: eval.updated_at,
            rescore_count: 0,
            history,
        };

        scheduler.enqueue_eval_job(job_id.clone(), pending).await;
        debug!(evaluation_id = %eval.id, %job_id, split_fetch, "eval job enqueued");
    }

    Ok(())
}

/// Build the eval `FlakeJob` and its `required_paths` from the evaluation's
/// recorded source. A `/nix/store/...` repository is an already-materialised
/// build-request source: dispatch it as `FlakeSource::Cached` (the worker
/// substitutes the NAR and evaluates via `path:`) instead of git-cloning it.
pub(crate) fn flake_job_for_eval_source(
    repository: &str,
    commit_sha: String,
    wildcards: Vec<String>,
    split_fetch: bool,
    input_overrides: Vec<gradient_types::proto::FlakeInputOverride>,
    input_update: Option<gradient_types::proto::InputUpdateSpec>,
) -> (FlakeJob, Vec<RequiredPath>) {
    use gradient_types::proto::FlakeSource;

    if repository.starts_with("/nix/store/") {
        let job = FlakeJob {
            tasks: vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations],
            source: FlakeSource::Cached {
                store_path: repository.to_owned(),
            },
            wildcards,
            timeout_secs: None,
            input_overrides,
            input_update,
        };
        let required = vec![RequiredPath {
            path: repository.to_owned(),
            cache_info: None,
        }];

        return (job, required);
    }

    let tasks = if split_fetch {
        vec![FlakeTask::FetchFlake]
    } else {
        vec![
            FlakeTask::FetchFlake,
            FlakeTask::EvaluateFlake,
            FlakeTask::EvaluateDerivations,
        ]
    };
    let job = FlakeJob {
        tasks,
        source: FlakeSource::Repository {
            url: repository.to_owned(),
            commit: commit_sha,
        },
        wildcards,
        timeout_secs: None,
        input_overrides,
        input_update,
    };

    (job, Vec::new())
}

// ── Build dispatch ────────────────────────────────────────────────────────────

async fn build_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let cancel = scheduler.state.shutdown.token();
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
/// then queried in-memory by [`BuildDispatchMaps::make_pending_job`].
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
    substitute_miss_escalation_threshold: i64,
    connected_architectures: HashSet<String>,
    build_retry_backoff_secs: u64,
    default_timeout_secs: Option<u64>,
    default_max_silent_secs: Option<u64>,
}

impl BuildDispatchMaps {
    /// Issue one IN-list query per table and build all lookup maps.
    async fn load(
        state: &Arc<ServerState>,
        anchors: &[MDerivationBuild],
        uses_history: bool,
        connected_architectures: HashSet<String>,
    ) -> anyhow::Result<Self> {
        let default_timeout_secs = nonzero(state.config.eval.build_default_timeout_secs);
        let default_max_silent_secs = nonzero(state.config.eval.build_default_max_silent_secs);

        let drv_ids: Vec<DerivationId> = anchors.iter().map(|a| a.derivation).collect();
        let anchor_ids: Vec<DerivationBuildId> = anchors.iter().map(|a| a.id).collect();

        let db = &state.worker_db;
        let substitute_misses = gradient_db::substitute_miss_counts(db, &anchor_ids)
            .await
            .unwrap_or_default();

        // Resolve the eval driving each anchor's dispatch: any referencing
        // build_job, preferring one whose evaluation is not terminal. The driving
        // eval is the job's peer-routing source and the build_job attributed on win.
        let mut driving_eval: HashMap<DerivationBuildId, EvaluationId> = HashMap::new();
        let mut jobs_by_anchor: HashMap<DerivationBuildId, Vec<EvaluationId>> = HashMap::new();
        for anchor in anchors {
            let jobs = gradient_db::build_jobs_for_derivation(db, anchor.derivation)
                .await
                .unwrap_or_default();
            jobs_by_anchor.insert(anchor.id, jobs.iter().map(|j| j.evaluation).collect());
        }

        let eval_ids: Vec<EvaluationId> = jobs_by_anchor
            .values()
            .flatten()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let derivations: HashMap<DerivationId, MDerivation> = gradient_db::fetch_in_chunks(
            &drv_ids,
            |chunk| async move { EDerivation::find().filter(CDerivation::Id.is_in(chunk)).all(db).await },
        )
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

        let evaluations: HashMap<EvaluationId, MEvaluation> = gradient_db::fetch_in_chunks(
            &eval_ids,
            |chunk| async move { EEvaluation::find().filter(CEvaluation::Id.is_in(chunk)).all(db).await },
        )
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

        // peer_id resolution: every evaluation must belong to a project.
        let project_ids: Vec<ProjectId> = evaluations
            .values()
            .filter_map(|e| e.project)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let projects: HashMap<ProjectId, OrganizationId> = gradient_db::fetch_in_chunks(
            &project_ids,
            |chunk| async move { EProject::find().filter(CProject::Id.is_in(chunk)).all(db).await },
        )
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
        .await
        .unwrap_or_default();
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
            .await
            .unwrap_or_default()
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
        .await
        .unwrap_or_default();

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
                .await
                .unwrap_or_default();
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
            .await
            .unwrap_or_default()
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

        // Closure sizes and historical predictions are only consumed by
        // policies that read history (resource-aware). Under the simple policy
        // we skip the walk entirely and use whatever is already persisted.
        // Derivations missing `closure_size` are sized in one batched walk
        // rather than one DB walk per derivation, then backfilled.
        let mut closure_sizes: HashMap<DerivationId, Option<i64>> = HashMap::new();
        let mut histories: HashMap<DerivationId, gradient_score::HistoryPrediction> = HashMap::new();
        if uses_history {
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
                    .col_expr(CDerivation::ClosureSize, sea_orm::sea_query::Expr::value(*size))
                    .filter(CDerivation::Id.eq(*drv_id))
                    .exec(&state.worker_db)
                    .await
                {
                    error!(derivation_id = %drv_id, error = %e, "failed to persist closure size");
                }
            }
            for (drv_id, drv) in &derivations {
                let size = drv.closure_size.or_else(|| computed.get(drv_id).copied());
                closure_sizes.insert(*drv_id, size);
                if let Some(pname) = &drv.pname {
                    let prediction =
                        crate::history::predict(&state.worker_db, pname, size).await;
                    histories.insert(*drv_id, prediction);
                }
            }
        } else {
            for (drv_id, drv) in &derivations {
                closure_sizes.insert(*drv_id, drv.closure_size);
            }
        }

        Ok(Self {
            derivations,
            evaluations,
            projects,
            features_by_drv,
            feature_names,
            dep_counts,
            direct_inputs,
            closure_sizes,
            histories,
            substitute_misses,
            driving_eval,
            substitute_miss_escalation_threshold: state
                .config
                .eval
                .substitute_miss_escalation_threshold as i64,
            connected_architectures,
            build_retry_backoff_secs: state.config.eval.build_retry_backoff_secs,
            default_timeout_secs,
            default_max_silent_secs,
        })
    }

    /// Resolve the organization that owns this evaluation (used as `peer_id`
    /// to route the job only to workers registered by that org).
    fn resolve_peer_id(&self, eval: &MEvaluation) -> Option<OrganizationId> {
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

    /// Assemble a `(job_id, PendingBuildJob)` pair for `anchor`, or `None` if
    /// any required map lookup fails (logged as errors at call site).
    fn make_pending_job(&self, anchor: &MDerivationBuild) -> Option<(String, PendingBuildJob)> {
        let derivation = self.derivations.get(&anchor.derivation).or_else(|| {
            error!(derivation_build = %anchor.id, "derivation not found for anchor");
            None
        })?;
        let eval_id = self.driving_eval.get(&anchor.id).copied().or_else(|| {
            error!(derivation_build = %anchor.id, "no driving evaluation for anchor");
            None
        })?;
        let eval = self.evaluations.get(&eval_id).or_else(|| {
            error!(derivation_build = %anchor.id, "driving evaluation row not found");
            None
        })?;
        let peer_id = self.resolve_peer_id(eval).or_else(|| {
            error!(derivation_build = %anchor.id, "could not resolve peer_id for anchor");
            None
        })?;

        let job_id = format!("build:{}", anchor.id);
        let miss_count = self
            .substitute_misses
            .get(&(anchor.id, eval_id))
            .copied()
            .unwrap_or(0);
        let arch_has_worker = arch_available(&self.connected_architectures, &derivation.architecture);
        // A fixed-output derivation is intrinsically substitutable regardless of
        // the anchor flag, so anchors created before this was recorded still
        // substitute instead of rebuilding their fetcher.
        let substitutable = anchor_substitutable(
            anchor.substitutable,
            derivation.is_fixed_output,
            derivation.allow_substitutes,
        );
        let mode = decide_dispatch_mode(
            substitutable,
            miss_count,
            self.substitute_miss_escalation_threshold,
            arch_has_worker,
        );

        if mode == BuildDispatchMode::SubstituteStalled
            && !crate::build::retry_backoff_elapsed(
                miss_count as i32,
                anchor.updated_at,
                now(),
                self.build_retry_backoff_secs,
            )
        {
            debug!(derivation_build = %anchor.id, "substitute stalled (no arch worker); backing off re-probe");
            return None;
        }

        let substitute = matches!(
            mode,
            BuildDispatchMode::SubstituteBuiltin | BuildDispatchMode::SubstituteStalled
        );
        // The worker round-trips this anchor uuid as the opaque BuildTask.build_id.
        let build_job = BuildJob {
            builds: vec![BuildTask {
                build_id: anchor.id.to_string(),
                drv_path: derivation.store_path(),
                external_cached: substitute,
                timeout_secs: resolve_limit(anchor.timeout_secs, self.default_timeout_secs),
                max_silent_secs: resolve_limit(anchor.max_silent_secs, self.default_max_silent_secs),
            }],
        };
        let (architecture, required_features) = if substitute {
            ("builtin".to_string(), Vec::new())
        } else {
            (derivation.architecture.clone(), self.required_features(anchor.derivation))
        };

        let pending = PendingBuildJob {
            derivation_build: anchor.id,
            evaluation_id: eval_id,
            peer_id,
            job: build_job,
            required_paths: self
                .direct_inputs
                .get(&anchor.derivation)
                .cloned()
                .unwrap_or_default(),
            architecture,
            required_features,
            dependency_count: self.dep_counts.get(&anchor.derivation).copied().unwrap_or(0),
            closure_size: self.closure_sizes.get(&anchor.derivation).copied().flatten(),
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

        Some((job_id, pending))
    }
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

    // Ready anchors: a global `derivation_build` is Queued, some `build_job`
    // still references its derivation (reachable from a surviving evaluation),
    // and every dependency anchor over `derivation_dependency` is terminal-
    // success (Completed or Substituted). The reachability check skips anchors
    // left Queued after their last referencing eval was torn down, which have
    // no driving evaluation to attribute the build to. Ordered by dependency
    // count desc (integration builds first), then by age.
    let anchors_sql = sea_orm::Statement::from_string(
        sea_orm::DbBackend::Postgres,
        format!(
            r#"
            SELECT db.*
            FROM public.derivation_build db
            WHERE db.status = {queued}
              AND EXISTS (
                  SELECT 1 FROM public.build_job bj WHERE bj.derivation = db.derivation
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM public.derivation_dependency dep_edge
                  WHERE dep_edge.derivation = db.derivation
                    AND NOT EXISTS (
                        SELECT 1
                        FROM public.derivation_build dep_db
                        WHERE dep_db.derivation = dep_edge.dependency
                          AND dep_db.status IN ({completed}, {substituted})
                    )
              )
            ORDER BY
                (SELECT count(*)
                   FROM public.derivation_dependency dd
                  WHERE dd.derivation = db.derivation) DESC,
                db.updated_at ASC
        "#,
            queued = i32::from(BuildStatus::Queued),
            completed = i32::from(BuildStatus::Completed),
            substituted = i32::from(BuildStatus::Substituted),
        ),
    );

    let started = std::time::Instant::now();
    let anchors = EDerivationBuild::find()
        .from_raw_sql(anchors_sql)
        .all(&state.worker_db)
        .await?;
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
            .col_expr(CDerivationBuild::ReadyAt, sea_orm::sea_query::Expr::value(now()))
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
        let Some((job_id, pending)) = maps.make_pending_job(&anchor) else {
            continue;
        };
        scheduler.enqueue_build_job(job_id, pending).await;
        enqueued += 1;
    }
    info!(
        enqueued,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "dispatch_ready_builds completed"
    );

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

pub(crate) async fn organization_id_for_eval(
    state: &Arc<ServerState>,
    eval: &MEvaluation,
) -> Option<OrganizationId> {
    let project_id = eval.project.or_else(|| {
        error!(evaluation_id = %eval.id, "evaluation has no project");
        None
    })?;
    match EProject::find_by_id(project_id).one(&state.worker_db).await {
        Ok(Some(p)) => Some(p.organization),
        Ok(None) => None,
        Err(e) => {
            error!(error = %e, %project_id, "failed to load project for eval");
            None
        }
    }
}

#[cfg(test)]
mod dispatch_mode_tests {
    use crate::dispatch_mode::{decide_dispatch_mode, BuildDispatchMode};

    #[test]
    fn stalled_substitute_stays_builtin() {
        assert_eq!(decide_dispatch_mode(true, 2, 2, false), BuildDispatchMode::SubstituteStalled);
        assert_eq!(decide_dispatch_mode(true, 2, 2, true), BuildDispatchMode::RealArch);
    }
}

#[cfg(test)]
mod eval_source_tests {
    use super::flake_job_for_eval_source;
    use gradient_types::proto::{FlakeSource, FlakeTask};

    #[test]
    fn cached_source_dispatches_without_fetch() {
        let (job, required) = flake_job_for_eval_source(
            "/nix/store/qgzxagd5bql1iqx0w8qzljwdlb06sn6n-source",
            "0".repeat(40),
            vec!["*".into()],
            false,
            vec![],
            None,
        );
        assert!(matches!(job.source, FlakeSource::Cached { .. }));
        assert_eq!(
            job.tasks,
            vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations]
        );
        assert!(!job.tasks.contains(&FlakeTask::FetchFlake));
        assert_eq!(required.len(), 1);
        assert_eq!(
            required[0].path,
            "/nix/store/qgzxagd5bql1iqx0w8qzljwdlb06sn6n-source"
        );
    }

    #[test]
    fn repository_source_keeps_fetch() {
        let (job, required) = flake_job_for_eval_source(
            "git@github.com:org/repo.git",
            "abc".into(),
            vec!["*".into()],
            false,
            vec![],
            None,
        );
        assert!(matches!(job.source, FlakeSource::Repository { .. }));
        assert!(job.tasks.contains(&FlakeTask::FetchFlake));
        assert!(required.is_empty());
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
