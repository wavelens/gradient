/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use chrono::Utc;
use core::database::add_features;
use core::executer::*;
use core::types::*;
use entity::build::BuildStatus;
use nix_daemon::nix::DaemonStore;
use sea_orm::ActiveValue::Set;
use sea_orm::entity::prelude::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;
use tracing::{debug, error, trace};
use uuid::Uuid;

use super::nix_commands::get_features_cmd;

/// Accumulates builds, dependency edges, and entry-point IDs across derivation processing.
pub(super) struct EvaluationAccumulator {
    pub(super) builds: Vec<MBuild>,
    pub(super) dependencies: Vec<MBuildDependency>,
    pub(super) entry_point_build_ids: Vec<Uuid>,
}

impl EvaluationAccumulator {
    pub(super) fn new() -> Self {
        Self {
            builds: vec![],
            dependencies: vec![],
            entry_point_build_ids: vec![],
        }
    }
}

/// Queries builds by derivation path within an organization, optionally filtering to only
/// `Completed` builds.
pub(super) async fn find_builds(
    state: Arc<ServerState>,
    organization_id: Uuid,
    build_paths: Vec<String>,
    successful: bool,
) -> Result<Vec<MBuild>> {
    let mut condition = Condition::any();
    for path in build_paths {
        condition = condition.add(CBuild::DerivationPath.eq(path.as_str()));
    }

    let mut filter = Condition::all()
        .add(CProject::Organization.eq(organization_id))
        .add(condition);

    if successful {
        filter = filter.add(CBuild::Status.eq(BuildStatus::Completed));
    }

    let builds = EBuild::find()
        .join(JoinType::InnerJoin, RBuild::Evaluation.def())
        .join(JoinType::InnerJoin, REvaluation::Project.def())
        .filter(filter)
        .all(&state.db)
        .await
        .context("Failed to query builds")?;

    Ok(builds)
}

/// Inserts a build that is already present in the Nix store as `Completed`, recording its outputs.
pub(super) async fn add_existing_build(
    state: Arc<ServerState>,
    derivation: String,
    evaluation_id: Uuid,
    build_id: Uuid,
) -> Result<MBuild> {
    let (system, features) =
        get_features_cmd(state.cli.binpath_nix.as_str(), derivation.as_str()).await?;

    let abuild = ABuild {
        id: Set(build_id),
        evaluation: Set(evaluation_id),
        derivation_path: Set(derivation.clone()),
        architecture: Set(system),
        status: Set(BuildStatus::Completed),
        server: Set(None),
        created_at: Set(Utc::now().naive_utc()),
        updated_at: Set(Utc::now().naive_utc()),
    };

    let build = abuild
        .insert(&state.db)
        .await
        .context("Failed to insert build")?;

    if let Err(e) = add_features(Arc::clone(&state), features, Some(build.id), None).await {
        error!(error = %e, "Failed to add features for build");
    }

    let local_store = get_local_store(None)
        .await
        .context("Failed to get local store")?;

    let full_derivation = core::executer::nix_store_path(&derivation);
    let outputs = match local_store {
        LocalNixStore::UnixStream(mut store) => {
            core::executer::get_build_outputs_from_derivation(full_derivation.clone(), &mut store).await
        }
        LocalNixStore::CommandDuplex(mut store) => {
            core::executer::get_build_outputs_from_derivation(full_derivation, &mut store).await
        }
    };

    if let Ok(outputs) = outputs {
        for output in outputs {
            let has_artefacts = tokio::fs::metadata(
                format!("{}/nix-support/hydra-build-products", output.path)
            ).await.is_ok();

            let abuild_output = ABuildOutput {
                id: Set(Uuid::new_v4()),
                build: Set(build.id),
                name: Set(output.name.clone()),
                output: Set(output.path.clone()),
                hash: Set(output.hash),
                package: Set(output.package),
                file_hash: Set(None),
                file_size: Set(None),
                is_cached: Set(false),
                has_artefacts: Set(has_artefacts),
                ca: Set(output.ca),
                created_at: Set(Utc::now().naive_utc()),
            };

            abuild_output
                .insert(&state.db)
                .await
                .context("Failed to insert build output")?;
        }
    }

    Ok(build)
}

/// BFS traversal over a build's dependency graph.
///
/// Newly discovered builds are accumulated in `acc.builds` and `acc.dependencies`.
/// Builds already present in the Nix store are recorded as `Completed`. Failed or aborted builds
/// that are missing from the store are cloned as fresh `Created` records so their dependency
/// graph can be re-traversed under the new ID.
///
/// All new builds start as `Created` (not `Queued`) to prevent the scheduler from picking them
/// up before the bulk dependency insert completes. The caller is responsible for transitioning
/// them to `Queued` after the insert.
pub(super) async fn query_all_dependencies<C: AsyncWriteExt + AsyncReadExt + Unpin + Send>(
    state: Arc<ServerState>,
    acc: &mut EvaluationAccumulator,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    dependencies: Vec<String>,
    store: &mut DaemonStore<C>,
) -> Result<()> {
    let mut dependencies = dependencies
        .into_iter()
        .map(|d| (d, None, Uuid::new_v4()))
        .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

    let mut pending: Vec<(String, Uuid)> = Vec::new();
    let mut pending_paths: HashSet<String> = HashSet::new();
    let mut reused_build_ids: HashSet<Uuid> = HashSet::new();

    while let Some((dependency, dependency_id, build_id)) = dependencies.pop() {
        debug!(
            derivation = %dependency,
            build_id = %build_id,
            parent_dependency_id = ?dependency_id,
            "Processing derivation"
        );

        let path_info = get_pathinfo(nix_store_path(&dependency), store)
            .await
            .context("Failed to get derivation info")?
            .context("Derivation not found in Nix store")?;

        // Strip /nix/store/ prefix from references so they match the format stored in the DB.
        let stripped_references: Vec<String> = path_info
            .references
            .iter()
            .map(|r| strip_nix_store_prefix(r))
            .collect();

        // TODO: can be optimized by also using Aborted and Failed builds.
        let already_exists = find_builds(
            Arc::clone(&state),
            organization_id,
            stripped_references.clone(),
            true,
        )
        .await
        .context("Failed to find existing builds")?;

        let mut references = stripped_references
            .into_iter()
            .map(|d| (d, Some(build_id), Uuid::new_v4()))
            .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

        let mut check_availability: Vec<MBuild> = Vec::new();

        references.retain(|d| {
            let d_path = d.0.clone();

            let in_builds = acc.builds.iter().any(|b| b.derivation_path == d_path)
                || pending_paths.contains(&d_path);
            let in_exists = already_exists.iter().find(|b| b.derivation_path == d_path);
            let in_dependencies = dependencies.iter().any(|(path, _, _)| *path == d_path);

            if in_builds || in_dependencies {
                let d_id = if let Some(b) = acc.builds.iter().find(|b| b.derivation_path == d_path)
                {
                    b.id
                } else if let Some((_, id)) = pending.iter().find(|(p, _)| *p == d_path) {
                    *id
                } else {
                    match dependencies.iter().find(|(path, _, _)| *path == d_path) {
                        Some((_, _, id)) => *id,
                        None => {
                            error!("Dependency not found for path: {}", d_path);
                            return false;
                        }
                    }
                };

                let dep = MBuildDependency {
                    id: Uuid::new_v4(),
                    build: build_id,
                    dependency: d_id,
                };

                debug!(build = %build_id, dependency = %d_id, "Creating dependency");

                acc.dependencies.push(dep);

                false
            } else if let Some(in_exists) = in_exists {
                check_availability.push(in_exists.clone());
                true
            } else {
                true
            }
        });

        for b in check_availability {
            references.retain(|(d, _, _)| *d != b.derivation_path);

            if get_missing_builds(vec![b.derivation_path.clone()], store)
                .await
                .context("Failed to get missing builds")?
                .is_empty()
            {
                // Already in the store — mark as Completed and reuse.
                let dep = MBuildDependency {
                    id: Uuid::new_v4(),
                    build: build_id,
                    dependency: b.id,
                };

                trace!(build = %build_id, dependency = %b.id, "Reusing existing build (in store)");

                let mut abuild: ABuild = b.clone().into();
                if b.status != BuildStatus::Completed {
                    abuild.status = Set(BuildStatus::Completed);
                }

                abuild.evaluation = Set(evaluation.id);
                abuild
                    .save(&state.db)
                    .await
                    .context("Failed to save build status")?;

                acc.dependencies.push(dep);
            } else {
                // Not in nix store — clone the failed/aborted build as a fresh record so
                // its history is preserved. Sub-dependencies are re-traversed under the
                // new ID via the BFS queue.
                // Use Created (not Queued) so the scheduler cannot pick this build up
                // before the new dependency records are bulk-inserted (race condition fix).
                let new_build_id = Uuid::new_v4();
                let now = Utc::now().naive_utc();
                let abuild = ABuild {
                    id: Set(new_build_id),
                    evaluation: Set(evaluation.id),
                    derivation_path: Set(b.derivation_path.clone()),
                    architecture: Set(b.architecture.clone()),
                    status: Set(BuildStatus::Created),
                    server: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                };

                abuild
                    .insert(&state.db)
                    .await
                    .context("Failed to insert cloned build")?;

                // Copy build features from the original build to the new one.
                let old_features = EBuildFeature::find()
                    .filter(CBuildFeature::Build.eq(b.id))
                    .all(&state.db)
                    .await
                    .context("Failed to query build features for cloning")?;

                for feat in old_features {
                    let af = ABuildFeature {
                        id: Set(Uuid::new_v4()),
                        build: Set(new_build_id),
                        feature: Set(feat.feature),
                    };
                    af.insert(&state.db)
                        .await
                        .context("Failed to copy build feature")?;
                }

                reused_build_ids.insert(new_build_id);

                references.push((b.derivation_path.clone(), Some(build_id), new_build_id));

                trace!(old_build = %b.id, new_build = %new_build_id, "Cloned failed/aborted build as new record for re-evaluation");
            }
        }

        let not_missing = get_missing_builds(vec![dependency.clone()], store)
            .await
            .context("Failed to get missing builds")?
            .is_empty();

        if not_missing {
            add_existing_build(
                Arc::clone(&state),
                dependency.clone(),
                evaluation.id,
                build_id,
            )
            .await?;

            debug!(
                build_id = %build_id,
                derivation_path = %dependency,
                "Skipping package - already in store"
            );
        } else {
            pending.push((dependency.clone(), build_id));
            pending_paths.insert(dependency.clone());
            dependencies.extend(references);
        };

        if let Some(d_id) = dependency_id {
            let dep = MBuildDependency {
                id: Uuid::new_v4(),
                build: d_id,
                dependency: build_id,
            };

            debug!(build = %d_id, dependency = %build_id, "Creating parent dependency");

            acc.dependencies.push(dep);
        }
    }

    // Spawn concurrent tasks to query architecture and features for all pending builds.
    type FeaturesResult = Result<(Uuid, String, entity::server::Architecture, Vec<String>)>;
    let handles: Vec<JoinHandle<FeaturesResult>> = pending
        .iter()
        .map(|(path, build_id)| {
            let binpath = state.cli.binpath_nix.clone();
            let p = path.clone();
            let id = *build_id;
            tokio::task::spawn(async move {
                let (arch, features) = get_features_cmd(binpath.as_str(), p.as_str())
                    .await
                    .with_context(|| {
                        format!("Failed to get build features for derivation: {}", p)
                    })?;
                Ok((id, p, arch, features))
            })
        })
        .collect();

    for handle in handles {
        let (build_id, path, system, features) =
            handle.await.context("get_features_cmd task panicked")??;

        if reused_build_ids.contains(&build_id) {
            // Already inserted into DB during BFS — skip re-insertion.
            continue;
        }

        if let Err(e) = add_features(Arc::clone(&state), features, Some(build_id), None).await {
            error!(error = %e, "Failed to add features for build");
        }

        // TODO: add better derivation check
        let build = if path.ends_with(".drv") {
            MBuild {
                id: build_id,
                evaluation: evaluation.id,
                derivation_path: path,
                architecture: system,
                status: BuildStatus::Created,
                server: None,
                created_at: Utc::now().naive_utc(),
                updated_at: Utc::now().naive_utc(),
            }
        } else {
            MBuild {
                id: build_id,
                evaluation: evaluation.id,
                derivation_path: path,
                architecture: system,
                status: BuildStatus::Completed,
                server: None,
                created_at: Utc::now().naive_utc(),
                updated_at: Utc::now().naive_utc(),
            }
        };

        debug!(
            build_id = %build.id,
            derivation_path = %build.derivation_path,
            "Creating build"
        );

        acc.builds.push(build);
    }

    Ok(())
}
