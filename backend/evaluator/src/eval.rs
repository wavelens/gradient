/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_core::input::{parse_evaluation_wildcard, repository_url_to_nix, vec_to_hex};
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Semaphore;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

/// Output tuple returned by `evaluate`:
///  - builds to insert
///  - newly-created derivations to insert
///  - newly-created derivation_output rows to insert
///  - newly-created derivation_dependency edges to insert
///  - entry-point (build_id, wildcard) pairs
///  - failed derivations: (drv, error_msg)
///  - pending features: (derivation_id, feature_names)
///  - evaluation warnings collected from the Nix evaluator
pub type EvaluationOutput = (
    Vec<MBuild>,
    Vec<MDerivation>,
    Vec<ADerivationOutput>,
    Vec<MDerivationDependency>,
    Vec<(Uuid, String)>,
    Vec<(String, String)>,
    Vec<(Uuid, Vec<String>)>,
    Vec<String>,
);

use gradient_core::status::{update_evaluation_status, update_evaluation_status_with_error};
use crate::dependencies::{SharedAccumulator, query_all_dependencies};

/// Evaluates a flake repository, discovering all matching derivations and building the dependency
/// graph. Returns accumulated builds, dependency edges, and the IDs of the top-level entry-point
/// builds (one per wildcard match).
#[instrument(skip(state), fields(evaluation_id = %evaluation.id))]
pub async fn evaluate(
    state: Arc<ServerState>,
    evaluation: &MEvaluation,
) -> Result<EvaluationOutput> {
    info!("Starting evaluation");

    // Fire-and-forget: the conditional update in update_evaluation_status
    // never clobbers terminal states, so spawning without awaiting is safe.
    // Saves one DB round-trip on the critical path.
    tokio::spawn(update_evaluation_status(
        Arc::clone(&state),
        evaluation.clone(),
        EvaluationStatus::EvaluatingFlake,
    ));

    // The commit query is independent of the org resolution chain —
    // run them concurrently to cut one DB round-trip.
    let (org_id_result, commit_result) = tokio::join!(
        resolve_organization_id(Arc::clone(&state), evaluation),
        ECommit::find_by_id(evaluation.commit).one(&state.db),
    );
    let organization_id = org_id_result?;
    let commit = commit_result
        .context("Failed to query commit")?
        .ok_or_else(|| anyhow::anyhow!("Commit not found"))?;

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .context("Failed to query organization")?
        .ok_or_else(|| anyhow::anyhow!("Organization not found"))?;

    let repository =
        repository_url_to_nix(&evaluation.repository, vec_to_hex(&commit.hash).as_str())
            .context("Failed to convert repository URL to Nix format")?;

    let _local_dir = state
        .flake_prefetcher
        .prefetch(
            state.cli.crypt_secret_file.clone(),
            state.cli.serve_url.clone(),
            repository.clone(),
            organization.clone(),
        )
        .await
        .context("Failed to prefetch flake")?;

    // For SSH repos the flake was cloned locally; use the path: URL so the Nix
    // C API never needs SSH credentials.  For HTTPS repos use the original URL.
    let nix_repository = match _local_dir.as_ref() {
        Some(prefetched) => format!("path:{}", prefetched.path.display()),
        None => repository.clone(),
    };

    let wildcards: Vec<String> = parse_evaluation_wildcard(evaluation.wildcard.as_str())
        .context("Failed to parse evaluation wildcard")?
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    let (all_derivations, mut eval_warnings) = state
        .derivation_resolver
        .list_flake_derivations(nix_repository.clone(), wildcards)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to evaluate: {}", e))?;

    if all_derivations.is_empty() {
        warn!("No derivations found for evaluation");
        return Ok((vec![], vec![], vec![], vec![], vec![], vec![], vec![], eval_warnings));
    }

    update_evaluation_status(
        Arc::clone(&state),
        evaluation.clone(),
        EvaluationStatus::EvaluatingDerivation,
    )
    .await;

    let shared = Arc::new(SharedAccumulator::new());
    let failed_derivations: Arc<StdMutex<Vec<(String, String)>>> =
        Arc::new(StdMutex::new(Vec::new()));
    let fatal_error: Arc<StdMutex<Option<anyhow::Error>>> = Arc::new(StdMutex::new(None));
    let total_derivations = all_derivations.len();

    let (resolved, resolve_warnings) = state
        .derivation_resolver
        .resolve_derivation_paths(nix_repository.clone(), all_derivations)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve derivation paths: {}", e))?;
    eval_warnings.extend(resolve_warnings);
    eval_warnings.sort_unstable();
    eval_warnings.dedup();

    // Fan the top-level closures out across a bounded pool of walkers.
    // Each walker's BFS runs concurrently; the SharedAccumulator
    // serialises the find-or-create of every derivation via per-path
    // OnceCells, so two walkers seeing the same transitive dep produce
    // exactly one `MDerivation` row.
    let parallelism = state.cli.eval_closure_parallelism.max(1);
    let semaphore = Arc::new(Semaphore::new(parallelism));

    let mut tasks: FuturesUnordered<_> = resolved
        .into_iter()
        .map(|(derivation_string, derivation_result)| {
            let state = Arc::clone(&state);
            let shared = Arc::clone(&shared);
            let failed = Arc::clone(&failed_derivations);
            let fatal = Arc::clone(&fatal_error);
            let semaphore = Arc::clone(&semaphore);
            let evaluation = evaluation.clone();
            async move {
                let _permit = semaphore.acquire_owned().await.ok();

                let derivation_path = match derivation_result {
                    Ok((d, _refs)) => d,
                    Err(e) => {
                        let error_msg = format!("{:#}", e);
                        warn!(
                            error = %error_msg,
                            derivation = %derivation_string,
                            "Derivation failed, skipping broken package"
                        );
                        failed.lock().unwrap().push((derivation_string, error_msg));
                        return;
                    }
                };

                info!(derivation = %derivation_path, "Walking derivation closure");

                let result = query_all_dependencies(
                    Arc::clone(&state),
                    Arc::clone(&shared),
                    &evaluation,
                    organization_id,
                    vec![derivation_path.clone()],
                )
                .await;

                match result {
                    Ok(()) => {
                        if let Some(root_build_id) = shared.lookup_build_for_path(&derivation_path)
                        {
                            shared.push_entry_point(root_build_id, derivation_string);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, derivation = %derivation_path, "Closure walk failed");
                        // Preserve the first fatal so the outer caller can
                        // surface it like the old sequential code did.
                        let mut slot = fatal.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                    }
                }
            }
        })
        .collect();

    while tasks.next().await.is_some() {}

    if let Some(e) = fatal_error.lock().unwrap().take() {
        return Err(e);
    }

    let (
        builds,
        new_derivations,
        new_derivation_outputs,
        new_derivation_dependencies,
        mut entry_point_build_ids,
        pending_features,
    ) = Arc::clone(&shared).into_parts();
    let failed_derivations = std::mem::take(&mut *failed_derivations.lock().unwrap());
    let warnings = eval_warnings;

    if builds.is_empty() && !failed_derivations.is_empty() {
        let error_summary = if failed_derivations.len() == total_derivations {
            format!(
                "All {} derivations failed during evaluation",
                total_derivations
            )
        } else {
            format!(
                "{} out of {} derivations failed, no builds created",
                failed_derivations.len(),
                total_derivations
            )
        };

        let detailed_errors: Vec<String> = failed_derivations
            .iter()
            .map(|(deriv, error)| format!("- {}: {}", deriv, error))
            .collect();

        let full_error = format!("{}:\n{}", error_summary, detailed_errors.join("\n"));
        return Err(anyhow::anyhow!(full_error));
    }

    let mut seen = std::collections::HashSet::new();
    entry_point_build_ids.retain(|(id, _)| seen.insert(*id));

    Ok((
        builds,
        new_derivations,
        new_derivation_outputs,
        new_derivation_dependencies,
        entry_point_build_ids,
        failed_derivations,
        pending_features,
        warnings,
    ))
}

/// Runs evaluation for a direct (non-repository) build using a local temp directory as the flake
/// source. Bulk-inserts resulting builds and dependencies, then transitions all `Created` builds
/// to `Queued` so the scheduler can pick them up.
pub async fn evaluate_direct(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    temp_dir: String,
) -> Result<()> {
    info!(evaluation_id = %evaluation.id, "Starting direct evaluation");

    let mut direct_evaluation = evaluation.clone();
    direct_evaluation.repository = temp_dir.clone();

    let evaluation_result = evaluate(Arc::clone(&state), &direct_evaluation).await;

    match evaluation_result {
        Ok((
            builds,
            new_derivations,
            new_derivation_outputs,
            new_derivation_dependencies,
            _entry_point_build_ids,
            _failed_derivations,
            pending_features,
            _warnings,
        )) => {
            info!(
                build_count = builds.len(),
                derivation_count = new_derivations.len(),
                dependency_count = new_derivation_dependencies.len(),
                "Direct evaluation completed successfully"
            );

            const BATCH_SIZE: usize = 1000;

            // Insert derivations first — builds FK into derivation.
            if !new_derivations.is_empty() {
                let active: Vec<ADerivation> = new_derivations
                    .iter()
                    .map(|d| d.clone().into_active_model())
                    .collect();
                for chunk in active.chunks(BATCH_SIZE) {
                    EDerivation::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert derivations")?;
                }
            }

            if !new_derivation_outputs.is_empty() {
                for chunk in new_derivation_outputs.chunks(BATCH_SIZE) {
                    EDerivationOutput::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert derivation outputs")?;
                }
            }

            if !new_derivation_dependencies.is_empty() {
                let active: Vec<ADerivationDependency> = new_derivation_dependencies
                    .iter()
                    .map(|d| d.clone().into_active_model())
                    .collect();
                for chunk in active.chunks(BATCH_SIZE) {
                    EDerivationDependency::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert derivation dependencies")?;
                }
            }

            // Builds go last: they FK into the just-inserted derivations.
            let active_builds = builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            if !active_builds.is_empty() {
                for chunk in active_builds.chunks(BATCH_SIZE) {
                    EBuild::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert builds")?;
                }
            }

            for (derivation_id, features) in pending_features {
                if let Err(e) = gradient_core::database::add_features(
                    Arc::clone(&state),
                    features,
                    Some(derivation_id),
                    None,
                )
                .await
                {
                    error!(error = %e, %derivation_id, "Failed to add features for direct derivation");
                }
            }

            // Transition all Created builds to Queued now that their dependency records are fully
            // inserted. This prevents the scheduler from racing against the bulk insert.
            let created_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(evaluation.id))
                .filter(CBuild::Status.eq(BuildStatus::Created))
                .all(&state.db)
                .await
                .unwrap_or_default();

            for build in created_builds {
                gradient_core::status::update_build_status(
                    Arc::clone(&state),
                    build,
                    BuildStatus::Queued,
                )
                .await;
            }

            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building)
                .await;

            if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
                warn!(error = %e, temp_dir = %temp_dir, "Failed to cleanup temp directory");
            }

            Ok(())
        }
        Err(e) => {
            error!(error = %e, "Direct evaluation failed");
            update_evaluation_status_with_error(
                Arc::clone(&state),
                evaluation,
                EvaluationStatus::Failed,
                format!("Direct evaluation failed: {}", e),
                Some("direct-eval".to_string()),
            )
            .await;

            if let Err(cleanup_err) = tokio::fs::remove_dir_all(&temp_dir).await {
                warn!(error = %cleanup_err, temp_dir = %temp_dir, "Failed to cleanup temp directory after evaluation failure");
            }

            Err(e)
        }
    }
}

/// Determines the organization that owns this evaluation (via project or direct build).
async fn resolve_organization_id(
    state: Arc<ServerState>,
    evaluation: &MEvaluation,
) -> Result<Uuid> {
    if let Some(project_id) = evaluation.project {
        let org = EProject::find_by_id(project_id)
            .one(&state.db)
            .await
            .context("Failed to query project")?
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?
            .organization;
        Ok(org)
    } else {
        let org = EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await
            .context("Failed to query direct build")?
            .ok_or_else(|| anyhow::anyhow!("Direct build not found"))?
            .organization;
        Ok(org)
    }
}
