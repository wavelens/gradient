/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod dependencies;
mod drv;
mod flake;
mod nix_commands;
mod nix_eval;

pub use drv::Derivation;
pub use nix_commands::{get_derivation, get_derivation_cmd, get_features};

use anyhow::{Context, Result};
use core::executer::*;
use core::input::{parse_evaluation_wildcard, repository_url_to_nix, vec_to_hex};
use core::sources::prefetch_flake;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use futures::stream::{self, StreamExt};
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

type ResolvedDerivation = (String, Result<(String, Vec<String>)>);
type EvaluationOutput = (Vec<MBuild>, Vec<MBuildDependency>, Vec<(Uuid, String)>, Vec<(String, String)>);

use dependencies::{add_existing_build, find_builds, query_all_dependencies, EvaluationAccumulator};
use flake::get_flake_derivations;
use super::scheduler::{update_evaluation_status, update_evaluation_status_with_error};

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
        EvaluationStatus::Evaluating,
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

    let _local_dir = prefetch_flake(Arc::clone(&state), repository.clone(), organization.clone())
        .await
        .context("Failed to prefetch flake")?;

    // For SSH repos the flake was cloned locally; use the path: URL so the Nix
    // C API never needs SSH credentials.  For HTTPS repos use the original URL.
    let nix_repository = match _local_dir.as_ref() {
        Some(dir) => format!("path:{}", dir.path().display()),
        None => repository.clone(),
    };

    let wildcards = parse_evaluation_wildcard(evaluation.wildcard.as_str())
        .context("Failed to parse evaluation wildcard")?;

    let all_derivations = get_flake_derivations(
        Arc::clone(&state),
        nix_repository.clone(),
        wildcards,
        organization.clone(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to evaluate: {}", e))?;

    if all_derivations.is_empty() {
        warn!("No derivations found for evaluation");
        return Ok((vec![], vec![], vec![], vec![]));
    }

    let mut acc = EvaluationAccumulator::new();
    let mut failed_derivations: Vec<(String, String)> = vec![];
    let total_derivations = all_derivations.len();

    // Resolve all derivation paths in parallel — each `nix path-info --derivation` call
    // is independent and CPU-bound, so running them concurrently uses all available cores.
    let concurrency = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let resolved: Vec<ResolvedDerivation> =
        stream::iter(all_derivations.into_iter())
            .map(|derivation_string| {
                let path = format!("{}#{}", nix_repository, derivation_string);
                async move {
                    let result = get_derivation_cmd(&path).await;
                    (derivation_string, result)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

    // Process resolved derivations sequentially (store and acc require exclusive access).
    for (derivation_string, derivation_result) in resolved {
        // TODO: use nix api
        let (derivation, _references) = match derivation_result {
            Ok((d, r)) => (d, r),
            Err(e) => {
                let error_msg = e.to_string();
                warn!(
                    error = %e,
                    derivation = %derivation_string,
                    "Derivation failed, skipping broken package"
                );
                failed_derivations.push((derivation_string.clone(), error_msg));
                continue;
            }
        };

        let missing = get_missing_builds(&state.nix_store_pool, vec![derivation.clone()]).await?;

        if missing.is_empty() {
            debug!(derivation = %derivation, "Skipping package - already in store");

            let build_id = Uuid::new_v4();
            match add_existing_build(
                Arc::clone(&state),
                derivation.clone(),
                evaluation.id,
                build_id,
            )
            .await
            {
                Ok(build) => acc.entry_point_build_ids.push((build.id, derivation_string.clone())),
                Err(e) => error!(error = %e, "Failed to add existing build"),
            }

            continue;
        }

        let already_exists = acc.builds.iter().any(|b| b.derivation_path == derivation);

        if already_exists {
            if let Some(existing) = acc.builds.iter().find(|b| b.derivation_path == derivation) {
                acc.entry_point_build_ids.push((existing.id, derivation_string.clone()));
            }
            debug!(derivation = %derivation, "Skipping package - already in current evaluation");
            continue;
        }

        let existing_builds =
            find_builds(Arc::clone(&state), organization_id, vec![derivation.clone()], true).await?;
        if let Some(existing) = existing_builds.first() {
            let missing = get_missing_builds(&state.nix_store_pool, vec![existing.derivation_path.clone()]).await?;
            if missing.is_empty() {
                acc.entry_point_build_ids.push((existing.id, derivation_string.clone()));
                debug!(derivation = %derivation, "Skipping package - already exists in DB and store");
                continue;
            }
            debug!(derivation = %derivation, "Completed build found in DB but missing from nix store, re-evaluating");
        }

        info!(derivation = %derivation, "Creating build");

        let entry_point_idx = acc.builds.len();
        query_all_dependencies(
            Arc::clone(&state),
            &mut acc,
            evaluation,
            organization_id,
            vec![derivation.clone()],
        )
        .await?;

        // The root build is the first one pushed during this call.
        if let Some(root) = acc.builds.get(entry_point_idx) {
            acc.entry_point_build_ids.push((root.id, derivation_string.clone()));
        }

        debug!(derivation = %derivation, "Successfully processed package");
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

    Ok((acc.builds, acc.dependencies, acc.entry_point_build_ids, failed_derivations))
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
        Ok((builds, dependencies, _entry_point_build_ids, _failed_derivations)) => {
            info!(
                build_count = builds.len(),
                dependency_count = dependencies.len(),
                "Direct evaluation completed successfully"
            );

            let active_builds = builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            let active_dependencies = dependencies
                .iter()
                .map(|d| d.clone().into_active_model())
                .collect::<Vec<ABuildDependency>>();

            if !active_builds.is_empty() {
                const BATCH_SIZE: usize = 1000;
                for chunk in active_builds.chunks(BATCH_SIZE) {
                    EBuild::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert builds")?;
                }
            }

            if !active_dependencies.is_empty() {
                const BATCH_SIZE: usize = 1000;
                for chunk in active_dependencies.chunks(BATCH_SIZE) {
                    EBuildDependency::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                        .context("Failed to insert dependencies")?;
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
