/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `EvalResult` messages from workers.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::evaluation_message::MessageLevel;
use gradient_core::db::{
    record_evaluation_message, update_build_status, update_evaluation_status,
    update_evaluation_status_with_error,
};
use gradient_core::sources::get_hash_from_path;
use gradient_core::types::*;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, error, info};

use super::build::check_evaluation_done;
use super::ci::report_ci_for_entry_points;
use super::jobs::PendingEvalJob;
use gradient_core::ci::CiStatus;
use gradient_core::types::proto::DiscoveredDerivation;

const BATCH_SIZE: usize = 1000;

// ── Derivation batch preparation ──────────────────────────────────────────────

/// New derivation rows and their outputs, ready for bulk DB insert.
struct DerivationInsertBatch {
    /// Mapping from drv_path → assigned UUID for all derivations (new + pre-existing).
    drv_path_to_id: HashMap<String, DerivationId>,
    new_derivations: Vec<ADerivation>,
    new_outputs: Vec<ADerivationOutput>,
}

impl DerivationInsertBatch {
    /// Build insert rows for derivations not yet in `existing`.
    fn prepare(
        organization_id: OrganizationId,
        derivations: &[DiscoveredDerivation],
        existing: &[MDerivation],
    ) -> Self {
        let mut drv_path_to_id: HashMap<String, DerivationId> = existing
            .iter()
            .map(|d| (d.derivation_path.clone(), d.id))
            .collect();

        let now = gradient_core::types::now();
        let mut new_derivations: Vec<ADerivation> = Vec::new();
        let mut new_outputs: Vec<ADerivationOutput> = Vec::new();

        for d in derivations {
            if drv_path_to_id.contains_key(&d.drv_path) {
                continue;
            }
            let id = DerivationId::now_v7();
            drv_path_to_id.insert(d.drv_path.clone(), id);
            new_derivations.push(ADerivation {
                id: Set(id),
                organization: Set(organization_id),
                derivation_path: Set(d.drv_path.clone()),
                architecture: Set(d.architecture.clone()),
                created_at: Set(now),
            });
            for output in &d.outputs {
                let (hash, package) = get_hash_from_path(output.path.clone())
                    .unwrap_or_else(|_| ("unknown".to_owned(), output.name.clone()));
                new_outputs.push(ADerivationOutput {
                    id: Set(DerivationOutputId::now_v7()),
                    derivation: Set(id),
                    name: Set(output.name.clone()),
                    output: Set(output.path.clone()),
                    hash: Set(hash),
                    package: Set(package),
                    ca: Set(None),
                    nar_size: Set(None),
                    is_cached: Set(false),
                    cached_path: Set(None),
                    created_at: Set(now),
                });
            }
        }

        Self {
            drv_path_to_id,
            new_derivations,
            new_outputs,
        }
    }

    /// Insert new derivations and outputs into the DB.
    ///
    /// Returns the `drv_path_to_id` map for downstream use.
    async fn insert(
        self,
        state: &Arc<ServerState>,
        evaluation: &MEvaluation,
    ) -> Result<HashMap<String, DerivationId>> {
        if !self.new_derivations.is_empty() {
            for chunk in self.new_derivations.chunks(BATCH_SIZE) {
                if let Err(e) = EDerivation::insert_many(chunk.to_vec())
                    .exec(&state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert derivations");
                    update_evaluation_status_with_error(
                        Arc::clone(state),
                        evaluation.clone(),
                        EvaluationStatus::Failed,
                        format!("failed to insert derivations: {}", e),
                        Some("db-insert".to_string()),
                    )
                    .await;
                    return Err(e.into());
                }
            }
        }
        if !self.new_outputs.is_empty() {
            for chunk in self.new_outputs.chunks(BATCH_SIZE) {
                if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec())
                    .exec(&state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert derivation outputs");
                }
            }
        }
        Ok(self.drv_path_to_id)
    }
}

// ── EvalResultProcessor ───────────────────────────────────────────────────────

/// Processes a single batch of derivations discovered during evaluation.
///
/// Holds the context shared by every step: server state, evaluation identity,
/// and the owning organisation. Created once in [`handle_eval_result`] and
/// passed through each pipeline stage.
struct EvalResultProcessor<'a> {
    state: &'a Arc<ServerState>,
    evaluation_id: EvaluationId,
    organization_id: OrganizationId,
    evaluation: MEvaluation,
}

impl<'a> EvalResultProcessor<'a> {
    fn new(
        state: &'a Arc<ServerState>,
        evaluation_id: EvaluationId,
        organization_id: OrganizationId,
        evaluation: MEvaluation,
    ) -> Self {
        Self {
            state,
            evaluation_id,
            organization_id,
            evaluation,
        }
    }

    /// Load derivations that already exist in the DB so we don't re-insert them.
    async fn load_existing_derivations(
        &self,
        derivations: &[DiscoveredDerivation],
    ) -> Result<Vec<MDerivation>> {
        let paths: Vec<String> = derivations.iter().map(|d| d.drv_path.clone()).collect();
        if paths.is_empty() {
            return Ok(vec![]);
        }
        EDerivation::find()
            .filter(CDerivation::Organization.eq(self.organization_id))
            .filter(CDerivation::DerivationPath.is_in(paths))
            .all(&self.state.worker_db)
            .await
            .context("query existing derivations")
    }

    /// Insert `ABuild` rows for each newly-discovered derivation.
    ///
    /// `Substituted` status is decided server-side from the actual cache
    /// state — a drv is Substituted iff *every* `derivation_output` row for
    /// it links to a `cached_path` whose `file_hash IS NOT NULL`. The
    /// worker's `substituted` flag is treated as a hint for its own
    /// scheduling and is ignored here, so a stale or lying `cached_path`
    /// row can never make us skip a build whose bytes aren't actually
    /// retrievable.
    async fn insert_build_rows(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) -> Result<()> {
        let now = gradient_core::types::now();
        let mut builds: Vec<ABuild> = Vec::new();

        let all_drv_ids: Vec<DerivationId> = derivations
            .iter()
            .filter_map(|d| drv_path_to_id.get(&d.drv_path).copied())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let truly_substituted = self.compute_truly_substituted(&all_drv_ids).await?;

        let buildable_drv_ids: Vec<DerivationId> = all_drv_ids
            .iter()
            .copied()
            .filter(|id| !truly_substituted.contains(id))
            .collect();

        let leader_for_drv = if buildable_drv_ids.is_empty() {
            HashMap::new()
        } else {
            find_active_leaders(self.state, &buildable_drv_ids).await
        };

        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
            // Three-way classification:
            //  - In our cache (`truly_substituted`)  → Substituted, no work
            //  - Worker says cached but our cache lacks it → upstream-only;
            //    Created + external_cached so the worker fetches from
            //    upstream and pushes to our cache instead of rebuilding
            //  - Otherwise → plain rebuild
            let (status, external_cached) = if truly_substituted.contains(&drv_id) {
                (BuildStatus::Substituted, false)
            } else if d.substituted {
                (BuildStatus::Created, true)
            } else {
                (BuildStatus::Created, false)
            };

            let via = if matches!(status, BuildStatus::Substituted) {
                None
            } else {
                leader_for_drv.get(&drv_id).copied()
            };

            builds.push(ABuild {
                id: Set(BuildId::now_v7()),
                evaluation: Set(self.evaluation_id),
                derivation: Set(drv_id),
                status: Set(status),
                log_id: Set(None),
                build_time_ms: Set(None),
                worker: Set(None),
                via: Set(via),
                external_cached: Set(external_cached),
                created_at: Set(now),
                updated_at: Set(now),
            });
        }

        if !builds.is_empty() {
            for chunk in builds.chunks(BATCH_SIZE) {
                if let Err(e) = EBuild::insert_many(chunk.to_vec())
                    .exec(&self.state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert builds");
                    update_evaluation_status_with_error(
                        Arc::clone(self.state),
                        self.evaluation.clone(),
                        EvaluationStatus::Failed,
                        format!("failed to insert builds: {}", e),
                        Some("db-insert".to_string()),
                    )
                    .await;
                    return Err(e.into());
                }
            }
        }

        Ok(())
    }

    /// Return the subset of `drv_ids` whose every `derivation_output` row
    /// links to a `cached_path` with `file_hash IS NOT NULL`. These are the
    /// drvs the server can confidently mark `Substituted` — anything else
    /// must be built (or substituted later when the bytes show up).
    async fn compute_truly_substituted(
        &self,
        drv_ids: &[DerivationId],
    ) -> Result<std::collections::HashSet<DerivationId>> {
        if drv_ids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }

        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(drv_ids.to_vec()))
            .all(&self.state.worker_db)
            .await
            .context("compute_truly_substituted: load derivation_output")?;

        if outputs.is_empty() {
            return Ok(std::collections::HashSet::new());
        }

        let cached_path_ids: Vec<CachedPathId> = outputs
            .iter()
            .filter_map(|o| o.cached_path)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let fully_cached_ids: std::collections::HashSet<CachedPathId> = if cached_path_ids.is_empty() {
            std::collections::HashSet::new()
        } else {
            ECachedPath::find()
                .filter(CCachedPath::Id.is_in(cached_path_ids))
                .all(&self.state.worker_db)
                .await
                .context("compute_truly_substituted: load cached_path")?
                .into_iter()
                .filter(|cp| cp.is_fully_cached())
                .map(|cp| cp.id)
                .collect()
        };

        // Group outputs by drv. A drv is truly substituted iff it has at
        // least one output AND every output is linked to a fully-cached
        // cached_path. Drvs without any derivation_output rows yet (eval
        // racing with another worker) fall through as not-substituted.
        let mut outputs_by_drv: HashMap<DerivationId, Vec<&MDerivationOutput>> = HashMap::new();
        for o in &outputs {
            outputs_by_drv.entry(o.derivation).or_default().push(o);
        }

        let mut substituted = std::collections::HashSet::new();
        for (drv_id, outs) in outputs_by_drv {
            let all_present = !outs.is_empty()
                && outs.iter().all(|o| {
                    o.is_cached
                        && o.cached_path
                            .map(|cp| fully_cached_ids.contains(&cp))
                            .unwrap_or(false)
                });
            if all_present {
                substituted.insert(drv_id);
            }
        }
        Ok(substituted)
    }

    /// Record per-derivation system-feature requirements in the DB.
    async fn add_system_features(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) {
        for d in derivations {
            if d.required_features.is_empty() {
                continue;
            }
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
            if let Err(e) = gradient_core::db::add_features(
                Arc::clone(self.state),
                d.required_features.clone(),
                entity::feature::FeatureKind::Feature,
                Some(drv_id),
            )
            .await
            {
                error!(error = %e, %drv_id, "failed to add system features");
            }
        }
    }

    /// Persist Nix evaluation warnings and errors as evaluation messages.
    async fn record_eval_messages(&self, warnings: &[String], errors: &[String]) {
        for warning in warnings {
            record_evaluation_message(
                self.state,
                self.evaluation_id,
                MessageLevel::Warning,
                warning.clone(),
                Some("nix-eval".to_string()),
            )
            .await;
        }
        for error in errors {
            record_evaluation_message(
                self.state,
                self.evaluation_id,
                MessageLevel::Error,
                error.clone(),
                Some("nix-eval".to_string()),
            )
            .await;
        }
    }

    /// Insert project entry points, report CI status, and schedule GC.
    ///
    /// Only called when the evaluation belongs to a project (not a standalone
    /// one-shot eval).
    async fn process_entry_points(
        &self,
        project_id: ProjectId,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) {
        let now = gradient_core::types::now();

        // Build a lookup: derivation_uuid → build_uuid for this evaluation.
        let eval_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(self.evaluation_id))
            .all(&self.state.worker_db)
            .await
            .unwrap_or_default();
        let drv_id_to_build: HashMap<DerivationId, BuildId> =
            eval_builds.iter().map(|b| (b.derivation, b.id)).collect();

        let mut entry_points: Vec<(BuildId, String)> = Vec::new();
        let mut active_entry_points: Vec<AEntryPoint> = Vec::new();

        for d in derivations {
            // Only root derivations (with a non-empty attr) are entry points.
            if d.attr.is_empty() {
                continue;
            }
            if let Some(&drv_id) = drv_path_to_id.get(&d.drv_path)
                && let Some(&build_id) = drv_id_to_build.get(&drv_id)
            {
                entry_points.push((build_id, d.attr.clone()));
                active_entry_points.push(AEntryPoint {
                    id: Set(EntryPointId::now_v7()),
                    project: Set(project_id),
                    evaluation: Set(self.evaluation_id),
                    build: Set(build_id),
                    eval: Set(d.attr.clone()),
                    created_at: Set(now),
                    repo_check_id: Set(None),
                });
            }
        }

        if !active_entry_points.is_empty() {
            for chunk in active_entry_points.chunks(BATCH_SIZE) {
                if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                    .exec(&self.state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert entry points");
                }
            }
        }

        // CI reporting — report per-entry-point status as Pending.
        if !entry_points.is_empty() {
            let state_clone = Arc::clone(self.state);
            let repo = self.evaluation.repository.clone();
            let commit_id = self.evaluation.commit;
            let evaluation_id = self.evaluation_id;
            self.state.shutdown.spawn(async move {
                report_ci_for_entry_points(
                    state_clone,
                    project_id,
                    commit_id,
                    &repo,
                    evaluation_id,
                    &entry_points,
                    CiStatus::Pending,
                )
                .await;
            });
        }

        // GC: remove old evaluations beyond keep_evaluations for this project.
        if let Ok(Some(project)) = EProject::find_by_id(project_id).one(&self.state.worker_db).await {
            let gc_state = Arc::clone(self.state);
            let gc_keep = project.keep_evaluations as usize;
            self.state.shutdown.spawn(async move {
                if let Err(e) =
                    gradient_core::db::gc_project_evaluations(gc_state, project_id, gc_keep).await
                {
                    error!(error = %e, %project_id, "GC: per-project evaluation GC failed");
                }
            });
        }
    }
}

// ── Substituted-closure expansion ────────────────────────────────────────────

/// Walk the transitive dependency closure of every `Substituted` build in
/// `evaluation_id` and insert `Substituted` build rows for any reachable
/// derivation that does not yet have a build row in this evaluation.
///
/// Called in [`handle_eval_job_completed`] after `flush_deferred_deps` has
/// written all dependency edges, so the full graph is available.  This
/// ensures:
/// * The dependency-gating SQL in `dispatch_ready_builds` sees build rows for
///   every dep of every queued build, even when large subtrees were pruned by
///   the BFS known-derivation optimisation.
/// * The evaluation's build list in the UI reflects the complete dep tree.
async fn expand_substituted_closure(state: &Arc<ServerState>, evaluation_id: EvaluationId) -> Result<()> {
    use sea_orm::ConnectionTrait;

    // Recursive CTE: seed = direct deps of substituted builds in this eval;
    // recurse through derivation_dependency until the closure is exhausted.
    // The outer SELECT filters to derivations without a build row yet, and
    // tags each result with the kind of seed it descends from:
    //  - 'sub' — reached from a `Substituted` (status=7) build; outputs are
    //    in our cache, transitive deps are too (Nix substitution invariant)
    //  - 'ext' — reached from a `Created + external_cached = true` build;
    //    outputs are in upstream only, transitive deps too. New rows for
    //    these get inserted with `external_cached = true` so the worker
    //    fetches them from upstream rather than trying to rebuild.
    //
    // When a drv is reachable via both kinds of seed simultaneously, prefer
    // 'sub' (already retrievable from our cache).
    let find_sql = sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        r#"
        WITH RECURSIVE sub_closure(drv_id, kind) AS (
            SELECT DISTINCT dd.dependency,
                CASE WHEN b.status = 7 THEN 'sub' ELSE 'ext' END
            FROM build b
            JOIN derivation_dependency dd ON dd.derivation = b.derivation
            WHERE b.evaluation = $1
              AND (b.status = 7 OR b.external_cached = TRUE)
            UNION
            SELECT dd2.dependency, sc.kind
            FROM derivation_dependency dd2
            JOIN sub_closure sc ON sc.drv_id = dd2.derivation
        )
        SELECT sc.drv_id, MIN(sc.kind) AS kind
        FROM sub_closure sc
        WHERE NOT EXISTS (
            SELECT 1 FROM build WHERE derivation = sc.drv_id AND evaluation = $1
        )
        GROUP BY sc.drv_id
        "#,
        [evaluation_id.into_inner().into()],
    );

    let rows = state
        .worker_db
        .query_all(find_sql)
        .await
        .context("expand_substituted_closure: query")?;
    if rows.is_empty() {
        return Ok(());
    }

    let now = gradient_core::types::now();
    let builds: Vec<ABuild> = rows
        .iter()
        .filter_map(|row| {
            let drv_id: DerivationId = row.try_get::<uuid::Uuid>("", "drv_id").ok()?.into();
            let kind = row.try_get::<String>("", "kind").ok()?;
            let (status, external_cached) = if kind == "sub" {
                (BuildStatus::Substituted, false)
            } else {
                (BuildStatus::Created, true)
            };
            Some(ABuild {
                id: Set(BuildId::now_v7()),
                evaluation: Set(evaluation_id),
                derivation: Set(drv_id),
                status: Set(status),
                log_id: Set(None),
                build_time_ms: Set(None),
                worker: Set(None),
                via: Set(None),
                external_cached: Set(external_cached),
                created_at: Set(now),
                updated_at: Set(now),
            })
        })
        .collect();

    let count = builds.len();
    for chunk in builds.chunks(BATCH_SIZE) {
        if let Err(e) = EBuild::insert_many(chunk.to_vec()).exec(&state.worker_db).await {
            error!(error = %e, %evaluation_id, "expand_substituted_closure: failed to insert builds");
        }
    }
    info!(%evaluation_id, count, "substituted closure expanded");
    Ok(())
}

// ── Public handlers ───────────────────────────────────────────────────────────

pub async fn handle_eval_result(
    state: &Arc<ServerState>,
    job: &PendingEvalJob,
    derivations: Vec<DiscoveredDerivation>,
    warnings: Vec<String>,
    errors: Vec<String>,
) -> Result<()> {
    let evaluation_id = job.evaluation_id;
    let organization_id = job.peer_id;

    let current = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await
        .context("fetch evaluation")?;

    let evaluation = match current {
        Some(e) if e.status == EvaluationStatus::Aborted => {
            info!(%evaluation_id, "evaluation aborted; discarding worker result");
            return Ok(());
        }
        Some(e) => e,
        None => anyhow::bail!("evaluation {} not found", evaluation_id),
    };

    info!(
        %evaluation_id,
        derivation_count = derivations.len(),
        warning_count = warnings.len(),
        error_count = errors.len(),
        "processing eval result from worker",
    );

    let proc = EvalResultProcessor::new(state, evaluation_id, organization_id, evaluation);

    let existing = proc.load_existing_derivations(&derivations).await?;
    let batch = DerivationInsertBatch::prepare(organization_id, &derivations, &existing);
    let drv_path_to_id = batch.insert(state, &proc.evaluation).await?;

    // Dependency edges are NOT created here. The BFS walks roots→leaves, so
    // batch N may contain derivation A whose dep B lands in batch N+1. Instead,
    // deps are accumulated in `Scheduler::deferred_deps` and flushed in one
    // shot by `flush_deferred_deps` inside `handle_eval_job_completed`, when
    // every derivation row is guaranteed to be in the DB.

    proc.insert_build_rows(&derivations, &drv_path_to_id)
        .await?;

    proc.add_system_features(&derivations, &drv_path_to_id)
        .await;

    proc.record_eval_messages(&warnings, &errors).await;

    // Errors are stored as evaluation_message rows and will cause
    // check_evaluation_done to mark the evaluation as Failed once all
    // queued builds finish (or immediately if there are no builds at all).
    // Do NOT mark Failed here: a later batch or a previous batch may have
    // queued builds that should still run.

    if let Some(project_id) = job.project_id {
        proc.process_entry_points(project_id, &derivations, &drv_path_to_id)
            .await;
    }

    debug!(
        %evaluation_id,
        new_derivations = derivations.len(),
        "eval batch persisted; awaiting more batches"
    );
    Ok(())
}

pub async fn handle_eval_job_completed(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<()> {
    // Expand the transitive dependency closure of all Substituted builds so
    // the full dep tree has build rows. This runs before Created→Queued
    // promotion so the dispatch SQL's dep-gating sees a complete picture.
    if let Err(e) = expand_substituted_closure(state, evaluation_id).await {
        error!(error = %e, %evaluation_id, "expand_substituted_closure failed (non-fatal)");
    }

    // The worker is done sending batches, so the evaluation's build set is
    // now final. Promote every `Created` build to `Queued` so the dispatcher
    // can pick them up, then move the evaluation into `Building`.
    let created = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.eq(BuildStatus::Created))
        .all(&state.worker_db)
        .await
        .unwrap_or_default();
    let queued_now = created.len();
    for build in created {
        update_build_status(Arc::clone(state), build, BuildStatus::Queued).await;
    }

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await?
        && matches!(
            eval.status,
            EvaluationStatus::EvaluatingFlake | EvaluationStatus::EvaluatingDerivation
        )
    {
        info!(
            %evaluation_id,
            queued = queued_now,
            "eval job complete; promoting evaluation to Building"
        );
        update_evaluation_status(Arc::clone(state), eval, EvaluationStatus::Building).await;
    }

    // If every build was already terminal (e.g. all Substituted), close the
    // evaluation out via the shared decision function.
    check_evaluation_done(state, evaluation_id).await
}

/// Flush all deferred dependency edges for `evaluation_id`.
///
/// Called once from `handle_eval_job_completed` after every derivation row is
/// guaranteed to be in the DB. Resolves `(drv_path, Vec<dep_drv_path>)` pairs
/// to `(derivation_uuid, dep_uuid)` edges and inserts them in bulk.
pub async fn flush_deferred_deps(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    organization_id: OrganizationId,
    deferred: Vec<(String, Vec<String>)>,
) -> Result<()> {
    if deferred.is_empty() {
        return Ok(());
    }

    // Collect every unique drv_path mentioned (both as source and as dep).
    let all_paths: Vec<String> = {
        let mut set = std::collections::HashSet::new();
        for (src, deps) in &deferred {
            set.insert(src.clone());
            for d in deps {
                set.insert(d.clone());
            }
        }
        set.into_iter().collect()
    };

    // Single query to map drv_path → UUID.
    let drv_path_to_id: std::collections::HashMap<String, DerivationId> = EDerivation::find()
        .filter(CDerivation::Organization.eq(organization_id))
        .filter(CDerivation::DerivationPath.is_in(all_paths))
        .all(&state.worker_db)
        .await
        .context("flush_deferred_deps: query derivations")?
        .into_iter()
        .map(|d| (d.derivation_path, d.id))
        .collect();

    let mut edges: Vec<ADerivationDependency> = Vec::new();
    let mut unresolved = 0usize;
    for (src, deps) in &deferred {
        let Some(&src_id) = drv_path_to_id.get(src) else {
            unresolved += 1;
            continue;
        };
        for dep in deps {
            if let Some(&dep_id) = drv_path_to_id.get(dep) {
                edges.push(ADerivationDependency {
                    id: Set(DerivationDependencyId::now_v7()),
                    derivation: Set(src_id),
                    dependency: Set(dep_id),
                });
            } else {
                unresolved += 1;
            }
        }
    }

    if !edges.is_empty() {
        for chunk in edges.chunks(BATCH_SIZE) {
            if let Err(e) = EDerivationDependency::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        CDerivationDependency::Derivation,
                        CDerivationDependency::Dependency,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .do_nothing()
                .exec(&state.worker_db)
                .await
            {
                error!(error = %e, "flush_deferred_deps: failed to insert edges");
            }
        }
    }

    info!(
        %evaluation_id,
        inserted = edges.len(),
        unresolved,
        "flushed deferred dependency edges"
    );
    Ok(())
}

pub async fn handle_eval_job_failed(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    error: &str,
) -> Result<()> {
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await?
        && !matches!(
            eval.status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
    {
        update_evaluation_status_with_error(
            Arc::clone(state),
            eval,
            EvaluationStatus::Failed,
            error.to_owned(),
            Some("worker".to_string()),
        )
        .await;
    }
    Ok(())
}

/// Find the in-flight leader build for each derivation in `drv_ids`.
///
/// A build is a leader if its `via` is NULL and its status is non-terminal
/// (Created/Queued/Building). Returns the head of the via chain — i.e. an
/// existing follower's `via` value, or the leader's own id. New builds for
/// the same derivation should set `via` to this id, producing a flat fan-out
/// (no chains).
async fn find_active_leaders(
    state: &Arc<ServerState>,
    drv_ids: &[DerivationId],
) -> HashMap<DerivationId, BuildId> {
    let rows = match EBuild::find()
        .filter(CBuild::Derivation.is_in(drv_ids.to_vec()))
        .filter(CBuild::Status.is_in(vec![
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(&state.worker_db)
        .await
    {
        Ok(rs) => rs,
        Err(e) => {
            error!(error = %e, "failed to query active leaders");
            return HashMap::new();
        }
    };

    let mut out: HashMap<DerivationId, BuildId> = HashMap::new();
    for b in rows {
        let head = b.via.unwrap_or(b.id);
        out.entry(b.derivation)
            .and_modify(|cur| {
                if b.via.is_none() {
                    *cur = b.id;
                }
            })
            .or_insert(head);
    }
    out
}
