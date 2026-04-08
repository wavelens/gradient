/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod dependencies;
mod flake;
mod nix_commands;
mod nix_eval;
mod worker;
mod worker_pool;

pub use worker::run_eval_worker;
pub use worker_pool::WorkerPoolResolver;

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::input::{parse_evaluation_wildcard, repository_url_to_nix, vec_to_hex};
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

/// Output tuple returned by `evaluate`:
///  - builds to insert
///  - newly-created derivations to insert
///  - newly-created derivation_output rows to insert
///  - newly-created derivation_dependency edges to insert
///  - entry-point (build_id, wildcard) pairs
///  - failed derivations: (drv, error_msg)
///  - pending features: (derivation_id, feature_names)
type EvaluationOutput = (
    Vec<MBuild>,
    Vec<MDerivation>,
    Vec<ADerivationOutput>,
    Vec<MDerivationDependency>,
    Vec<(Uuid, String)>,
    Vec<(String, String)>,
    Vec<(Uuid, Vec<String>)>,
);

use super::scheduler::{update_evaluation_status, update_evaluation_status_with_error};
use dependencies::{EvaluationAccumulator, find_derivation, query_all_dependencies};

/// Evaluates a flake repository, discovering all matching derivations and building the dependency
/// graph. Returns accumulated builds, dependency edges, and the IDs of the top-level entry-point
/// builds (one per wildcard match).
#[instrument(skip(state), fields(evaluation_id = %evaluation.id))]
pub async fn evaluate(
    state: Arc<ServerState>,
    evaluation: &MEvaluation,
) -> Result<EvaluationOutput> {
    info!("Starting evaluation");
    update_evaluation_status(
        Arc::clone(&state),
        evaluation.clone(),
        EvaluationStatus::EvaluatingFlake,
    )
    .await;

    let organization_id = resolve_organization_id(Arc::clone(&state), evaluation).await?;

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .context("Failed to query organization")?
        .ok_or_else(|| anyhow::anyhow!("Organization not found"))?;

    let commit = ECommit::find_by_id(evaluation.commit)
        .one(&state.db)
        .await
        .context("Failed to query commit")?
        .ok_or_else(|| anyhow::anyhow!("Commit not found"))?;

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

    let all_derivations = state
        .derivation_resolver
        .list_flake_derivations(nix_repository.clone(), wildcards)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to evaluate: {}", e))?;

    if all_derivations.is_empty() {
        warn!("No derivations found for evaluation");
        return Ok((vec![], vec![], vec![], vec![], vec![], vec![], vec![]));
    }

    update_evaluation_status(
        Arc::clone(&state),
        evaluation.clone(),
        EvaluationStatus::EvaluatingDerivation,
    )
    .await;

    let mut acc = EvaluationAccumulator::new();
    let mut failed_derivations: Vec<(String, String)> = vec![];
    let total_derivations = all_derivations.len();

    let resolved = state
        .derivation_resolver
        .resolve_derivation_paths(nix_repository.clone(), all_derivations)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve derivation paths: {}", e))?;

    // Process resolved derivations sequentially (store and acc require exclusive access).
    for (derivation_string, derivation_result) in resolved {
        let (derivation_path, _references) = match derivation_result {
            Ok((d, r)) => (d, r),
            Err(e) => {
                let error_msg = format!("{:#}", e);
                warn!(
                    error = %error_msg,
                    derivation = %derivation_string,
                    "Derivation failed, skipping broken package"
                );
                failed_derivations.push((derivation_string.clone(), error_msg));
                continue;
            }
        };

        // Fast-path: if the derivation row already exists and we already
        // materialised a build for it in this evaluation, just attach the
        // entry point.
        let existing_derivation =
            find_derivation(&state, organization_id, &derivation_path).await?;
        if let Some(ref d) = existing_derivation
            && let Some(existing) = acc.builds.iter().find(|b| b.derivation == d.id)
        {
            acc.entry_point_build_ids
                .push((existing.id, derivation_string.clone()));
            debug!(derivation = %derivation_path, "Skipping — already in current evaluation");
            continue;
        }

        info!(derivation = %derivation_path, "Walking derivation closure");

        let before_builds = acc.builds.len();
        query_all_dependencies(
            Arc::clone(&state),
            &mut acc,
            evaluation,
            organization_id,
            vec![derivation_path.clone()],
        )
        .await?;

        // The root build is the first one pushed during this call.
        if let Some(root) = acc.builds.get(before_builds) {
            acc.entry_point_build_ids
                .push((root.id, derivation_string.clone()));
        }

        debug!(derivation = %derivation_path, "Successfully processed package");
    }

    if acc.builds.is_empty() && !failed_derivations.is_empty() {
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
    acc.entry_point_build_ids.retain(|(id, _)| seen.insert(*id));

    Ok((
        acc.builds,
        acc.new_derivations,
        acc.new_derivation_outputs,
        acc.new_derivation_dependencies,
        acc.entry_point_build_ids,
        failed_derivations,
        acc.pending_features,
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
                crate::scheduler::update_build_status(
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
