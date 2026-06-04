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
//! - `build_dispatch_loop`: finds ready `Queued` builds → enqueues `BuildJob`s
//!
//! The eval/build loops are idempotent: re-enqueueing the same job_id overwrites
//! the existing entry in the `JobTracker` without harm.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::sources::get_path_from_derivation_output;
use gradient_core::types::input::vec_to_hex;
use gradient_core::types::wildcard::Wildcard;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use tracing::{debug, error, info};

use super::Scheduler;
use super::jobs::{PendingBuildJob, PendingEvalJob};
use gradient_core::types::proto::{
    BuildJob, BuildTask, CacheInfo, FlakeJob, FlakeTask, RequiredPath,
};

/// Spawns all dispatch loops on the shared shutdown tracker so they drain on SIGTERM.
pub fn start_dispatch_loops(scheduler: Arc<Scheduler>) {
    let shutdown = scheduler.state.shutdown.clone();
    let s1 = Arc::clone(&scheduler);
    let s2 = Arc::clone(&scheduler);
    let s3 = Arc::clone(&scheduler);
    shutdown.spawn(async move { super::trigger_dispatch::trigger_dispatch_loop(s3).await });
    shutdown.spawn(async move { eval_dispatch_loop(s1).await });
    shutdown.spawn(async move { build_dispatch_loop(s2).await });
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
            use entity::evaluation_flake_input_override as efio;
            use sea_orm::QueryOrder;
            efio::Entity::find()
                .filter(efio::Column::Evaluation.eq(eval.id))
                .order_by_asc(efio::Column::InputName)
                .all(&state.worker_db)
                .await?
                .into_iter()
                .map(|r| gradient_core::types::proto::FlakeInputOverride {
                    input_name: r.input_name,
                    url: r.url,
                })
                .collect::<Vec<_>>()
        };

        let split_fetch = scheduler.worker_pool.read().await.has_idle_eval_only_worker();
        let tasks = if split_fetch {
            vec![FlakeTask::FetchFlake]
        } else {
            vec![
                FlakeTask::FetchFlake,
                FlakeTask::EvaluateFlake,
                FlakeTask::EvaluateDerivations,
            ]
        };

        let flake_job = FlakeJob {
            tasks,
            source: gradient_core::types::proto::FlakeSource::Repository {
                url: eval.repository.clone(),
                commit: commit_sha,
            },
            wildcards: eval
                .wildcard
                .parse::<Wildcard>()
                .map(|w| w.patterns().to_vec())
                .unwrap_or_else(|_| vec![eval.wildcard.clone()]),
            timeout_secs: None,
            input_overrides,
        };

        let organization_id = organization_id_for_eval(state, &eval).await;
        let org_id = match organization_id {
            Some(id) => id,
            None => {
                error!(evaluation_id = %eval.id, "could not determine organization for evaluation");
                continue;
            }
        };

        let pending = PendingEvalJob {
            evaluation_id: eval.id,
            project_id: eval.project,
            peer_id: org_id,
            commit_id: eval.commit,
            repository: eval.repository.clone(),
            job: flake_job,
            required_paths: vec![],
            queued_at: eval.updated_at,
        };

        scheduler.enqueue_eval_job(job_id.clone(), pending).await;
        debug!(evaluation_id = %eval.id, %job_id, split_fetch, "eval job enqueued");
    }

    Ok(())
}

// ── Build dispatch ────────────────────────────────────────────────────────────

async fn build_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let cancel = scheduler.state.shutdown.token();
    info!("build dispatch loop started");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("build dispatch loop shutting down");
                return;
            }
            _ = interval.tick() => {}
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

/// Move `FailedTransient` builds whose backoff window has elapsed back to
/// `Queued` so the ready-builds pass can dispatch them again.
pub(crate) async fn requeue_transient_failures(scheduler: &Scheduler) -> anyhow::Result<()> {
    use crate::build::retry_backoff_elapsed;
    let state = &scheduler.state;
    let base = state.config.eval.build_retry_backoff_secs;
    let now = gradient_core::types::now();

    let transient = EBuild::find()
        .filter(CBuild::Status.eq(BuildStatus::FailedTransient))
        .all(&state.worker_db)
        .await?;
    for build in transient {
        if retry_backoff_elapsed(build.attempt, build.updated_at, now, base) {
            gradient_core::db::update_build_status(Arc::clone(state), build, BuildStatus::Queued)
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
    histories: HashMap<DerivationId, score::HistoryPrediction>,
    default_timeout_secs: Option<u64>,
    default_max_silent_secs: Option<u64>,
}

impl BuildDispatchMaps {
    /// Issue one IN-list query per table and build all lookup maps.
    async fn load(
        state: &Arc<ServerState>,
        builds: &[MBuild],
        uses_history: bool,
    ) -> anyhow::Result<Self> {
        let default_timeout_secs = nonzero(state.config.eval.build_default_timeout_secs);
        let default_max_silent_secs = nonzero(state.config.eval.build_default_max_silent_secs);

        let drv_ids: Vec<DerivationId> = builds.iter().map(|b| b.derivation).collect();
        let eval_ids: Vec<EvaluationId> = builds
            .iter()
            .map(|b| b.evaluation)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let derivations: HashMap<DerivationId, MDerivation> = EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids.clone()))
            .all(&state.worker_db)
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect();

        let evaluations: HashMap<EvaluationId, MEvaluation> = EEvaluation::find()
            .filter(CEvaluation::Id.is_in(eval_ids))
            .all(&state.worker_db)
            .await?
            .into_iter()
            .map(|e| (e.id, e))
            .collect();

        // peer_id resolution: every evaluation must belong to a project.
        let project_ids: Vec<ProjectId> = evaluations
            .values()
            .filter_map(|e| e.project)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let projects: HashMap<ProjectId, OrganizationId> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            EProject::find()
                .filter(CProject::Id.is_in(project_ids))
                .all(&state.worker_db)
                .await?
                .into_iter()
                .map(|p| (p.id, p.organization))
                .collect()
        };

        // Required features: per-derivation list of feature names.
        let feature_edges = EDerivationFeature::find()
            .filter(CDerivationFeature::Derivation.is_in(drv_ids.clone()))
            .all(&state.worker_db)
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
            EFeature::find()
                .filter(CFeature::Id.is_in(feature_ids))
                .filter(CFeature::Kind.eq(entity::feature::FeatureKind::Feature))
                .all(&state.worker_db)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|f| (f.id, f.name))
                .collect()
        };

        // Direct dependency edges per derivation. Used both for the scoring
        // policy's `dep_counts` and to build `direct_inputs` below.
        let dep_edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(drv_ids.clone()))
            .all(&state.worker_db)
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
                let outs = EDerivationOutput::find()
                    .filter(CDerivationOutput::Derivation.is_in(dep_drv_ids))
                    .all(&state.worker_db)
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

        let cache_info_by_hash: HashMap<String, CacheInfo> = if output_hashes.is_empty() {
            HashMap::new()
        } else {
            ECachedPath::find()
                .filter(CCachedPath::Hash.is_in(output_hashes))
                .all(&state.worker_db)
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
                .collect()
        };

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
                        path: get_path_from_derivation_output(o.clone()),
                        cache_info,
                    });
                }
            }
            direct_inputs.insert(*drv_id, paths);
        }

        // Closure sizes: reuse the persisted `derivation.closure_size` when set;
        // otherwise compute the transitive output size once and backfill the row
        // so future dispatch passes skip the walk.
        let mut closure_sizes: HashMap<DerivationId, Option<i64>> = HashMap::new();
        for (drv_id, drv) in &derivations {
            let size = match drv.closure_size {
                Some(s) => Some(s),
                None => Self::backfill_closure_size(state, *drv_id).await,
            };
            closure_sizes.insert(*drv_id, size);
        }

        // Historical predictions: only loaded when the active policy reads
        // history, and only for derivations carrying a pname to match against.
        let mut histories: HashMap<DerivationId, score::HistoryPrediction> = HashMap::new();
        if uses_history {
            for (drv_id, drv) in &derivations {
                if let Some(pname) = &drv.pname {
                    let closure_size = closure_sizes.get(drv_id).copied().flatten();
                    let prediction =
                        crate::history::predict(&state.worker_db, pname, closure_size).await;
                    histories.insert(*drv_id, prediction);
                }
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
            default_timeout_secs,
            default_max_silent_secs,
        })
    }

    /// Compute the transitive closure size for `drv_id` and persist it onto the
    /// `derivation` row. Failures degrade to `None` (scoring treats it as
    /// unknown) and are retried on the next dispatch pass.
    async fn backfill_closure_size(
        state: &Arc<ServerState>,
        drv_id: DerivationId,
    ) -> Option<i64> {
        let size = match gradient_core::db::transitive_closure_size(&state.worker_db, &[drv_id]).await
        {
            Ok(s) => s,
            Err(e) => {
                error!(derivation_id = %drv_id, error = %e, "failed to compute closure size");
                return None;
            }
        };
        if let Err(e) = EDerivation::update_many()
            .col_expr(CDerivation::ClosureSize, sea_orm::sea_query::Expr::value(size))
            .filter(CDerivation::Id.eq(drv_id))
            .exec(&state.worker_db)
            .await
        {
            error!(derivation_id = %drv_id, error = %e, "failed to persist closure size");
        }
        Some(size)
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

    /// Assemble a `(job_id, PendingBuildJob)` pair for `build`, or `None` if
    /// any required map lookup fails (logged as errors at call site).
    fn make_pending_job(&self, build: &MBuild) -> Option<(String, PendingBuildJob)> {
        let derivation = self.derivations.get(&build.derivation).or_else(|| {
            error!(build_id = %build.id, "derivation not found for build");
            None
        })?;
        let eval = self.evaluations.get(&build.evaluation).or_else(|| {
            error!(build_id = %build.id, "evaluation not found for build");
            None
        })?;
        let peer_id = self.resolve_peer_id(eval).or_else(|| {
            error!(build_id = %build.id, "could not resolve peer_id for build");
            None
        })?;

        let job_id = format!("build:{}", build.id);
        // Placeholder; the real value is set per-assignment in
        // `job_handlers::apply_sign_flag` based on the receiving worker's
        // `sign` capability.
        let build_job = BuildJob {
            builds: vec![BuildTask {
                build_id: build.id.to_string(),
                drv_path: derivation.store_path(),
                external_cached: build.external_cached,
                timeout_secs: resolve_limit(build.timeout_secs, self.default_timeout_secs),
                max_silent_secs: resolve_limit(build.max_silent_secs, self.default_max_silent_secs),
            }],
        };
        let pending = PendingBuildJob {
            build_id: build.id,
            evaluation_id: build.evaluation,
            peer_id,
            job: build_job,
            required_paths: self
                .direct_inputs
                .get(&build.derivation)
                .cloned()
                .unwrap_or_default(),
            architecture: derivation.architecture.clone(),
            required_features: self.required_features(build.derivation),
            dependency_count: self.dep_counts.get(&build.derivation).copied().unwrap_or(0),
            closure_size: self.closure_sizes.get(&build.derivation).copied().flatten(),
            prefer_local_build: derivation.prefer_local_build,
            history: self
                .histories
                .get(&build.derivation)
                .copied()
                .unwrap_or_default(),
            queued_at: build.updated_at,
        };

        Some((job_id, pending))
    }
}

pub(crate) async fn dispatch_ready_builds(scheduler: &Scheduler) -> anyhow::Result<()> {
    let state = &scheduler.state;

    // Ready builds: status = Queued AND every dependency build is Completed or Substituted.
    // Ordered by dependency count desc (integration builds first), then by age.
    // Driven off the `idx-build-ready-queue` partial index for the outer
    // filter, and the `idx-build-evaluation-derivation` composite for the
    // double-`NOT EXISTS` antijoin (`for every dep edge, a Completed or
    // Substituted build exists in the same evaluation`).
    let builds_sql = sea_orm::Statement::from_string(
        sea_orm::DbBackend::Postgres,
        format!(
            r#"
            SELECT b.*
            FROM public.build b
            WHERE b.status = {queued}
              AND b.via IS NULL
              AND NOT EXISTS (
                  SELECT 1
                  FROM public.derivation_dependency dep_edge
                  WHERE dep_edge.derivation = b.derivation
                    AND NOT EXISTS (
                        SELECT 1
                        FROM public.build dep_build
                        WHERE dep_build.evaluation = b.evaluation
                          AND dep_build.derivation = dep_edge.dependency
                          AND dep_build.status IN ({completed}, {substituted})
                    )
              )
            ORDER BY
                (SELECT count(*)
                   FROM public.derivation_dependency dd
                  WHERE dd.derivation = b.derivation) DESC,
                b.updated_at ASC
        "#,
            queued = i32::from(BuildStatus::Queued),
            completed = i32::from(BuildStatus::Completed),
            substituted = i32::from(BuildStatus::Substituted),
        ),
    );

    let started = std::time::Instant::now();
    let builds = EBuild::find()
        .from_raw_sql(builds_sql)
        .all(&state.worker_db)
        .await?;
    if builds.is_empty() {
        return Ok(());
    }

    // Filter out builds already in the in-memory tracker - one lock acquisition
    // for the whole pass instead of per-build.
    let new_builds: Vec<MBuild> = {
        let tracker = scheduler.job_tracker.read().await;
        builds
            .into_iter()
            .filter(|b| !tracker.contains_job(&format!("build:{}", b.id)))
            .collect()
    };
    if new_builds.is_empty() {
        return Ok(());
    }

    let maps =
        BuildDispatchMaps::load(state, &new_builds, scheduler.policy.uses_history()).await?;

    let mut enqueued = 0usize;
    for build in new_builds {
        let Some((job_id, pending)) = maps.make_pending_job(&build) else {
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

async fn organization_id_for_eval(
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
