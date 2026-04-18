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
use uuid::Uuid;

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
    drv_path_to_id: HashMap<String, Uuid>,
    new_derivations: Vec<ADerivation>,
    new_outputs: Vec<ADerivationOutput>,
}

impl DerivationInsertBatch {
    /// Build insert rows for derivations not yet in `existing`.
    fn prepare(
        organization_id: Uuid,
        derivations: &[DiscoveredDerivation],
        existing: &[MDerivation],
    ) -> Self {
        let mut drv_path_to_id: HashMap<String, Uuid> = existing
            .iter()
            .map(|d| (d.derivation_path.clone(), d.id))
            .collect();

        let now = chrono::Utc::now().naive_utc();
        let mut new_derivations: Vec<ADerivation> = Vec::new();
        let mut new_outputs: Vec<ADerivationOutput> = Vec::new();

        for d in derivations {
            if drv_path_to_id.contains_key(&d.drv_path) {
                continue;
            }
            let id = Uuid::new_v4();
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
                    id: Set(Uuid::new_v4()),
                    derivation: Set(id),
                    name: Set(output.name.clone()),
                    output: Set(output.path.clone()),
                    hash: Set(hash),
                    package: Set(package),
                    ca: Set(None),
                    file_hash: Set(None),
                    file_size: Set(None),
                    nar_size: Set(None),
                    is_cached: Set(false),
                    has_artefacts: Set(false),
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
    ) -> Result<HashMap<String, Uuid>> {
        if !self.new_derivations.is_empty() {
            for chunk in self.new_derivations.chunks(BATCH_SIZE) {
                if let Err(e) = EDerivation::insert_many(chunk.to_vec())
                    .exec(&state.db)
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
                    .exec(&state.db)
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
    evaluation_id: Uuid,
    organization_id: Uuid,
    evaluation: MEvaluation,
}

impl<'a> EvalResultProcessor<'a> {
    fn new(
        state: &'a Arc<ServerState>,
        evaluation_id: Uuid,
        organization_id: Uuid,
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
            .all(&self.state.db)
            .await
            .context("query existing derivations")
    }

    /// Insert `ABuild` rows for each newly-discovered derivation.
    async fn insert_build_rows(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, Uuid>,
    ) -> Result<()> {
        let now = chrono::Utc::now().naive_utc();
        let mut builds: Vec<ABuild> = Vec::new();

        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
            let status = if d.substituted {
                BuildStatus::Substituted
            } else {
                BuildStatus::Created
            };
            builds.push(ABuild {
                id: Set(Uuid::new_v4()),
                evaluation: Set(self.evaluation_id),
                derivation: Set(drv_id),
                status: Set(status),
                server: Set(None),
                log_id: Set(None),
                build_time_ms: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            });
        }

        if !builds.is_empty() {
            for chunk in builds.chunks(BATCH_SIZE) {
                if let Err(e) = EBuild::insert_many(chunk.to_vec())
                    .exec(&self.state.db)
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

    /// Record per-derivation system-feature requirements in the DB.
    async fn add_system_features(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, Uuid>,
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
        project_id: Uuid,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, Uuid>,
    ) {
        let now = chrono::Utc::now().naive_utc();

        // Build a lookup: derivation_uuid → build_uuid for this evaluation.
        let eval_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(self.evaluation_id))
            .all(&self.state.db)
            .await
            .unwrap_or_default();
        let drv_id_to_build: HashMap<Uuid, Uuid> =
            eval_builds.iter().map(|b| (b.derivation, b.id)).collect();

        let mut entry_points: Vec<(Uuid, String)> = Vec::new();
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
                    id: Set(Uuid::new_v4()),
                    project: Set(project_id),
                    evaluation: Set(self.evaluation_id),
                    build: Set(build_id),
                    eval: Set(d.attr.clone()),
                    created_at: Set(now),
                });
            }
        }

        if !active_entry_points.is_empty() {
            for chunk in active_entry_points.chunks(BATCH_SIZE) {
                if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                    .exec(&self.state.db)
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
            tokio::spawn(async move {
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
        if let Ok(Some(project)) = EProject::find_by_id(project_id).one(&self.state.db).await {
            let gc_state = Arc::clone(self.state);
            let gc_keep = project.keep_evaluations as usize;
            tokio::spawn(async move {
                if let Err(e) =
                    gradient_core::db::gc_project_evaluations(gc_state, project_id, gc_keep).await
                {
                    error!(error = %e, %project_id, "GC: per-project evaluation GC failed");
                }
            });
        }
    }
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
        .one(&state.db)
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

    // If all attrs failed and nothing was resolved, mark evaluation as Failed.
    if derivations.is_empty() && !errors.is_empty() {
        update_evaluation_status_with_error(
            Arc::clone(state),
            proc.evaluation,
            EvaluationStatus::Failed,
            format!("{} attr(s) failed to resolve", errors.len()),
            Some("nix-eval".to_string()),
        )
        .await;
        return Ok(());
    }

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
    evaluation_id: Uuid,
) -> Result<()> {
    // The worker is done sending batches, so the evaluation's build set is
    // now final. Promote every `Created` build to `Queued` so the dispatcher
    // can pick them up, then move the evaluation into `Building`.
    let created = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.eq(BuildStatus::Created))
        .all(&state.db)
        .await
        .unwrap_or_default();
    let queued_now = created.len();
    for build in created {
        update_build_status(Arc::clone(state), build, BuildStatus::Queued).await;
    }

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
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
    evaluation_id: Uuid,
    organization_id: Uuid,
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
    let drv_path_to_id: std::collections::HashMap<String, Uuid> = EDerivation::find()
        .filter(CDerivation::Organization.eq(organization_id))
        .filter(CDerivation::DerivationPath.is_in(all_paths))
        .all(&state.db)
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
                    id: Set(Uuid::new_v4()),
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
                .exec(&state.db)
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
    evaluation_id: Uuid,
    error: &str,
) -> Result<()> {
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
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
