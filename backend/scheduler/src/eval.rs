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
    find_active_leaders, record_evaluation_message, update_build_status, update_evaluation_status,
    update_evaluation_status_with_error,
};
use gradient_core::executer::strip_nix_store_prefix;
use gradient_core::sources::{get_hash_from_path, parse_drv_hash_name};
use gradient_core::types::*;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use tracing::{debug, error, info};

use super::build::check_evaluation_done;
use super::jobs::PendingEvalJob;
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
        let mut drv_path_to_id: HashMap<String, DerivationId> =
            existing.iter().map(|d| (d.drv_path(), d.id)).collect();

        let now = gradient_core::types::now();
        let mut new_derivations: Vec<ADerivation> = Vec::new();
        let mut new_outputs: Vec<ADerivationOutput> = Vec::new();

        for d in derivations {
            if drv_path_to_id.contains_key(&d.drv_path) {
                continue;
            }
            let id = DerivationId::now_v7();
            drv_path_to_id.insert(d.drv_path.clone(), id);
            let (drv_hash, drv_name) = parse_drv_hash_name(&d.drv_path)
                .unwrap_or_else(|_| ("unknown".to_owned(), d.drv_path.clone()));
            new_derivations.push(ADerivation {
                id: Set(id),
                organization: Set(organization_id),
                hash: Set(drv_hash),
                name: Set(drv_name),
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
    ///
    /// Filters by `hash` only (Nix store hashes are content-addressed, so
    /// `(organization, hash)` is unique in practice) to keep the IN clause
    /// bounded by the number of distinct hashes rather than full drv paths.
    async fn load_existing_derivations(
        &self,
        derivations: &[DiscoveredDerivation],
    ) -> Result<Vec<MDerivation>> {
        let hashes: Vec<String> = derivations
            .iter()
            .filter_map(|d| parse_drv_hash_name(&d.drv_path).ok().map(|(h, _)| h))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if hashes.is_empty() {
            return Ok(vec![]);
        }
        EDerivation::find()
            .filter(CDerivation::Organization.eq(self.organization_id))
            .filter(CDerivation::Hash.is_in(hashes))
            .all(&self.state.worker_db)
            .await
            .context("query existing derivations")
    }

    /// Insert `ABuild` rows for each newly-discovered derivation.
    ///
    /// `Substituted` status is decided server-side from the actual cache
    /// state - a drv is Substituted iff *every* `derivation_output` row for
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

        let leader_for_drv = find_active_leaders(
            &self.state.worker_db,
            self.organization_id,
            &buildable_drv_ids,
        )
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "failed to query active leaders");
            HashMap::new()
        });

        let log_source_for_substituted = self
            .find_log_sources(truly_substituted.iter().copied().collect())
            .await;

        let mut spawn_inputs: Vec<(BuildId, DerivationId, String)> = Vec::new();
        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
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

            let log_id = if matches!(status, BuildStatus::Substituted) {
                log_source_for_substituted.get(&drv_id).copied()
            } else {
                None
            };

            let build_id = BuildId::now_v7();
            if matches!(status, BuildStatus::Substituted) {
                spawn_inputs.push((build_id, drv_id, d.drv_path.clone()));
            }

            builds.push(ABuild {
                id: Set(build_id),
                evaluation: Set(self.evaluation_id),
                derivation: Set(drv_id),
                status: Set(status),
                log_id: Set(log_id),
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

        for (build_id, drv_id, drv_path) in spawn_inputs {
            let state = Arc::clone(self.state);
            tokio::spawn(async move {
                if let Err(e) = crate::log_substitution::substitute_log(
                    state, build_id, drv_id, drv_path, false,
                )
                .await
                {
                    tracing::warn!(%build_id, error = %e, "substitute_log spawn failed");
                }
            });
        }

        Ok(())
    }

    /// Dispatch `build.substituted` for every just-inserted Substituted build
    /// that has an `entry_point` row. Substituted builds are inserted in
    /// their terminal state and never go through `update_build_status`, so
    /// the regular status-change dispatch path never fires for them.
    ///
    /// Must be called AFTER `process_entry_points` — the reporter skips
    /// build events without an `entry_point`, so dispatching before
    /// `entry_point` rows exist would silently drop every check.
    pub(crate) async fn dispatch_substituted_events(&self) -> Result<(), sea_orm::DbErr> {
        use gradient_core::db::status::dispatch_build_event_for_status;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let substituted = entity::build::Entity::find()
            .filter(entity::build::Column::Evaluation.eq(self.evaluation_id))
            .filter(entity::build::Column::Status.eq(BuildStatus::Substituted))
            .all(&self.state.worker_db)
            .await?;

        for build in substituted {
            dispatch_build_event_for_status(self.state, build, BuildStatus::Substituted).await;
        }
        Ok(())
    }

    /// Return the subset of `drv_ids` whose every `derivation_output` row's
    /// hash matches a `cached_path` with `file_hash IS NOT NULL`. These are
    /// the drvs the server can confidently mark `Substituted` - anything
    /// else must be built (or substituted later when the bytes show up).
    ///
    /// Matching is by hash rather than the explicit
    /// `derivation_output.cached_path` link because that link is set lazily
    /// by `mark_nar_stored` on upload. A new drv whose output hash is
    /// already in `cached_path` (shared FOD source, re-evaluated build
    /// before its first upload, manual cache push) would otherwise be
    /// misclassified as "needs to build" and rerun pointlessly. The
    /// worker-facing `CacheQuery` handler already merges by hash for the
    /// same reason; this brings the eval-time decision in line.
    /// For each derivation being marked `Substituted`, find the most recent
    /// prior build that has a usable log so the new build's `log_id` can
    /// point at it. A new Substituted build never runs and so produces no
    /// log of its own; without this lookup the log endpoint sees
    /// `log_id = NULL` and falls back to the new build's id, which has no
    /// stored log.
    async fn find_log_sources(&self, drv_ids: Vec<DerivationId>) -> HashMap<DerivationId, BuildId> {
        let mut out: HashMap<DerivationId, BuildId> = HashMap::new();
        if drv_ids.is_empty() {
            return out;
        }
        let prior = match EBuild::find()
            .filter(CBuild::Derivation.is_in(drv_ids))
            .filter(CBuild::Status.is_in([BuildStatus::Completed, BuildStatus::Substituted]))
            .order_by_desc(CBuild::CreatedAt)
            .all(&self.state.worker_db)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "find_log_sources query failed");
                return out;
            }
        };
        for b in prior {
            let log_key = b.log_id.unwrap_or(b.id);
            out.entry(b.derivation).or_insert(log_key);
        }
        out
    }

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

        let hashes: Vec<String> = outputs
            .iter()
            .map(|o| o.hash.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let fully_cached_hashes: std::collections::HashSet<String> = ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(hashes))
            .all(&self.state.worker_db)
            .await
            .context("compute_truly_substituted: load cached_path")?
            .into_iter()
            .filter(|cp| cp.is_fully_cached())
            .map(|cp| cp.hash)
            .collect();

        let mut outputs_by_drv: HashMap<DerivationId, Vec<&MDerivationOutput>> = HashMap::new();
        for o in &outputs {
            outputs_by_drv.entry(o.derivation).or_default().push(o);
        }

        let mut substituted = std::collections::HashSet::new();
        for (drv_id, outs) in outputs_by_drv {
            let all_present =
                !outs.is_empty() && outs.iter().all(|o| fully_cached_hashes.contains(&o.hash));
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

    /// Insert project entry points and schedule per-project evaluation GC.
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

        let mut active_entry_points: Vec<AEntryPoint> = Vec::new();

        for d in derivations {
            if d.attr.is_empty() {
                continue;
            }
            if let Some(&drv_id) = drv_path_to_id.get(&d.drv_path)
                && let Some(&build_id) = drv_id_to_build.get(&drv_id)
            {
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

        // GC: remove old evaluations beyond keep_evaluations for this project.
        if let Ok(Some(project)) = EProject::find_by_id(project_id)
            .one(&self.state.worker_db)
            .await
        {
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
async fn expand_substituted_closure(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<()> {
    use sea_orm::ConnectionTrait;

    // Recursive CTE: seed = direct deps of substituted builds in this eval;
    // recurse through derivation_dependency until the closure is exhausted.
    // The outer SELECT filters to derivations without a build row yet, and
    // tags each result with the kind of seed it descends from:
    //  - 'sub' - reached from a `Substituted` (status=7) build; outputs are
    //    in our cache, transitive deps are too (Nix substitution invariant)
    //  - 'ext' - reached from a `Created + external_cached = true` build;
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
    let mut builds: Vec<ABuild> = Vec::with_capacity(rows.len());
    let mut spawn_inputs: Vec<(BuildId, DerivationId)> = Vec::new();
    for row in &rows {
        let Ok(drv_id_uuid) = row.try_get::<uuid::Uuid>("", "drv_id") else {
            continue;
        };
        let drv_id: DerivationId = drv_id_uuid.into();
        let Ok(kind) = row.try_get::<String>("", "kind") else {
            continue;
        };
        let (status, external_cached) = if kind == "sub" {
            (BuildStatus::Substituted, false)
        } else {
            (BuildStatus::Created, true)
        };
        let build_id = BuildId::now_v7();
        if matches!(status, BuildStatus::Substituted) {
            spawn_inputs.push((build_id, drv_id));
        }
        builds.push(ABuild {
            id: Set(build_id),
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
        });
    }

    let count = builds.len();
    for chunk in builds.chunks(BATCH_SIZE) {
        if let Err(e) = EBuild::insert_many(chunk.to_vec())
            .exec(&state.worker_db)
            .await
        {
            error!(error = %e, %evaluation_id, "expand_substituted_closure: failed to insert builds");
        }
    }

    if !spawn_inputs.is_empty() {
        let drv_ids: Vec<DerivationId> = spawn_inputs.iter().map(|(_, d)| *d).collect();
        let paths = match EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids))
            .all(&state.worker_db)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|d| (d.id, d.drv_path()))
                .collect::<std::collections::HashMap<_, _>>(),
            Err(e) => {
                error!(%evaluation_id, error = %e, "expand_substituted_closure: drv path lookup failed");
                std::collections::HashMap::new()
            }
        };

        for (build_id, drv_id) in spawn_inputs {
            let Some(drv_path) = paths.get(&drv_id).cloned() else {
                continue;
            };
            let state = Arc::clone(state);
            tokio::spawn(async move {
                if let Err(e) = crate::log_substitution::substitute_log(
                    state, build_id, drv_id, drv_path, false,
                )
                .await
                {
                    tracing::warn!(%build_id, error = %e, "substitute_log spawn failed");
                }
            });
        }
    }

    info!(%evaluation_id, count, "substituted closure expanded");
    Ok(())
}

// ── Public handlers ───────────────────────────────────────────────────────────

pub async fn handle_eval_result(
    state: &Arc<ServerState>,
    job: &PendingEvalJob,
    mut derivations: Vec<DiscoveredDerivation>,
    warnings: Vec<String>,
    errors: Vec<String>,
) -> Result<()> {
    // `derivation.derivation_path` stores the bare `<hash>-<name>` form
    // (narinfo `References:` convention). Callers may pass either form;
    // normalise once here so every downstream key (existing-rows map,
    // insert path, build-row lookup, deferred deps) is in canonical form.
    for d in &mut derivations {
        d.drv_path = strip_nix_store_prefix(&d.drv_path);
        for dep in &mut d.dependencies {
            *dep = strip_nix_store_prefix(dep);
        }
    }

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

    // Must run after `process_entry_points`: the forge reporter skips build
    // events without a matching `entry_point` row, so emitting
    // `build.substituted` earlier would drop every check.
    if let Err(e) = proc.dispatch_substituted_events().await {
        tracing::warn!(error = %e, "failed to dispatch build.substituted events");
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

    // Collect every unique drv_path mentioned (both as source and as dep), and
    // derive the unique hash set we'll filter the DB on. Hashes are
    // content-addressed (32-char nix32) so filtering by hash alone is enough to
    // pin a row down within an organization.
    let mut all_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (src, deps) in &deferred {
        all_paths.insert(src.clone());
        for d in deps {
            all_paths.insert(d.clone());
        }
    }
    let all_hashes: Vec<String> = all_paths
        .iter()
        .filter_map(|p| parse_drv_hash_name(p).ok().map(|(h, _)| h))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let drv_path_to_id: std::collections::HashMap<String, DerivationId> = EDerivation::find()
        .filter(CDerivation::Organization.eq(organization_id))
        .filter(CDerivation::Hash.is_in(all_hashes))
        .all(&state.worker_db)
        .await
        .context("flush_deferred_deps: query derivations")?
        .into_iter()
        .map(|d| (d.drv_path(), d.id))
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
