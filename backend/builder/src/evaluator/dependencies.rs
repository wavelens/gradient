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
use futures::stream::{FuturesUnordered, StreamExt};
use nix_daemon::PathInfo;
use sea_orm::ActiveValue::Set;
use sea_orm::entity::prelude::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, error, trace};
use uuid::Uuid;

use super::nix_commands::get_features;

/// Accumulates builds, dependency edges, entry-point IDs, and deferred features across
/// derivation processing. Features are deferred because `build_feature` has a FK to `build`,
/// so features can only be inserted after the builds are persisted to the DB.
pub(super) struct EvaluationAccumulator {
    pub(super) builds: Vec<MBuild>,
    pub(super) dependencies: Vec<MBuildDependency>,
    pub(super) entry_point_build_ids: Vec<(Uuid, String)>,
    /// Features to be inserted after builds are bulk-inserted into the DB.
    pub(super) pending_features: Vec<(Uuid, Vec<String>)>,
}

impl EvaluationAccumulator {
    pub(super) fn new() -> Self {
        Self {
            builds: vec![],
            dependencies: vec![],
            entry_point_build_ids: vec![],
            pending_features: vec![],
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
    let (system, features) = get_features(derivation.as_str()).await?;

    let abuild = ABuild {
        id: Set(build_id),
        evaluation: Set(evaluation_id),
        derivation_path: Set(derivation.clone()),
        architecture: Set(system),
        status: Set(BuildStatus::Completed),
        server: Set(None),
        log_id: Set(Some(build_id)),
        build_time_ms: Set(None),
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

    let mut local_store = state
        .nix_store_pool
        .acquire()
        .await
        .context("Failed to acquire local store")?;

    let full_derivation = core::executer::nix_store_path(&derivation);
    let outputs = core::executer::get_build_outputs_from_derivation(full_derivation, &mut *local_store).await;

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
                last_fetched_at: Set(None),
            };

            abuild_output
                .insert(&state.db)
                .await
                .context("Failed to insert build output")?;
        }
    }

    Ok(build)
}

/// Returns the build ID for a known derivation path, searching the accumulator,
/// the pending-build list, and the BFS queue in that order.
fn resolve_known_build_id(
    path: &str,
    acc: &EvaluationAccumulator,
    pending_builds: &[(String, Uuid)],
    queue: &[(String, Option<Uuid>, Uuid)],
) -> Option<Uuid> {
    acc.builds
        .iter()
        .find(|b| b.derivation_path == path)
        .map(|b| b.id)
        .or_else(|| pending_builds.iter().find(|(p, _)| p == path).map(|(_, id)| *id))
        .or_else(|| queue.iter().find(|(p, _, _)| p == path).map(|(_, _, id)| *id))
}

/// Clones a failed or aborted build into a fresh `Created` record for re-evaluation.
/// Features are copied from the original. The new ID is tracked in `cloned_ids` so
/// `finalize_pending_builds` can skip re-inserting it.
async fn clone_failed_build(
    state: &Arc<ServerState>,
    evaluation: &MEvaluation,
    original: &MBuild,
    cloned_ids: &mut HashSet<Uuid>,
) -> Result<Uuid> {
    let new_id = Uuid::new_v4();
    let now = Utc::now().naive_utc();

    ABuild {
        id: Set(new_id),
        evaluation: Set(evaluation.id),
        derivation_path: Set(original.derivation_path.clone()),
        architecture: Set(original.architecture.clone()),
        status: Set(BuildStatus::Created),
        server: Set(None),
        log_id: Set(Some(new_id)),
        build_time_ms: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&state.db)
    .await
    .context("insert cloned build")?;

    let features = EBuildFeature::find()
        .filter(CBuildFeature::Build.eq(original.id))
        .all(&state.db)
        .await
        .context("query features for clone")?;

    for feat in features {
        ABuildFeature {
            id: Set(Uuid::new_v4()),
            build: Set(new_id),
            feature: Set(feat.feature),
        }
        .insert(&state.db)
        .await
        .context("copy build feature")?;
    }

    trace!(old_build = %original.id, new_build = %new_id, "Cloned failed build for re-evaluation");
    cloned_ids.insert(new_id);
    Ok(new_id)
}

/// Marks an existing build as `Completed` under the current evaluation and records
/// the dependency edge from `parent_build_id` → `build.id`.
async fn reuse_existing_build(
    state: &Arc<ServerState>,
    evaluation: &MEvaluation,
    build: &MBuild,
    parent_build_id: Uuid,
    acc: &mut EvaluationAccumulator,
) -> Result<()> {
    let mut abuild: ABuild = build.clone().into();
    if build.status != BuildStatus::Completed {
        abuild.status = Set(BuildStatus::Completed);
    }

    abuild.evaluation = Set(evaluation.id);
    abuild.save(&state.db).await.context("save reused build")?;

    trace!(build = %parent_build_id, dependency = %build.id, "Reusing existing build (in store)");
    acc.dependencies.push(MBuildDependency {
        id: Uuid::new_v4(),
        build: parent_build_id,
        dependency: build.id,
    });
    Ok(())
}

/// Processes one derivation node during the BFS:
///
/// 1. Classifies each reference as "already tracked", "exists in DB", or "new".
/// 2. Issues a **single** `get_missing_builds` call for all paths that need
///    store-presence verification (DB-existing refs + the current derivation).
/// 3. Resolves each case: reuse in-store builds, clone evicted builds, queue new ones.
/// 4. Records the parent → this dependency edge.
#[allow(clippy::too_many_arguments)]
async fn process_node(
    state: &Arc<ServerState>,
    acc: &mut EvaluationAccumulator,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    queue: &mut Vec<(String, Option<Uuid>, Uuid)>,
    pending_builds: &mut Vec<(String, Uuid)>,
    pending_paths: &mut HashSet<String>,
    cloned_ids: &mut HashSet<Uuid>,
    derivation: String,
    parent_id: Option<Uuid>,
    build_id: Uuid,
    path_info: PathInfo,
) -> Result<()> {
    let references: Vec<String> = path_info
        .references
        .iter()
        .map(|r| strip_nix_store_prefix(r))
        .collect();

    // Find which references already have completed build records in the DB.
    let db_builds = find_builds(Arc::clone(state), organization_id, references.clone(), true)
        .await
        .context("find existing builds for references")?;

    // Classify each reference into one of three buckets.
    let mut new_refs: Vec<(String, Option<Uuid>, Uuid)> = Vec::new();
    let mut check_avail: Vec<MBuild> = Vec::new();

    for ref_path in references {
        if let Some(known_id) = resolve_known_build_id(&ref_path, acc, pending_builds, queue) {
            // Already tracked — just record the dependency edge.
            debug!(build = %build_id, dependency = %known_id, "Dependency edge to known build");
            acc.dependencies.push(MBuildDependency {
                id: Uuid::new_v4(),
                build: build_id,
                dependency: known_id,
            });
        } else if let Some(existing) = db_builds.iter().find(|b| b.derivation_path == ref_path) {
            // Exists in DB — need to verify it is still in the Nix store.
            check_avail.push(existing.clone());
        } else {
            // Completely new derivation.
            new_refs.push((ref_path, Some(build_id), Uuid::new_v4()));
        }
    }

    // Single batched store-presence check for all check_avail paths + this derivation.
    let paths_to_check: Vec<String> = check_avail
        .iter()
        .map(|b| b.derivation_path.clone())
        .chain(std::iter::once(derivation.clone()))
        .collect();

    let missing: HashSet<String> = get_missing_builds(&state.nix_store_pool, paths_to_check)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Resolve each DB-existing reference against the store result.
    for existing in check_avail {
        if missing.contains(&existing.derivation_path) {
            // Evicted — clone as a fresh build so its sub-deps are re-traversed.
            let new_id = clone_failed_build(state, evaluation, &existing, cloned_ids).await?;
            new_refs.push((existing.derivation_path, Some(build_id), new_id));
        } else {
            // Still present — reuse and record the dependency edge.
            reuse_existing_build(state, evaluation, &existing, build_id, acc).await?;
        }
    }

    // Decide fate of the current derivation.
    if missing.contains(&derivation) {
        // Needs building — add to pending and extend the queue with its new references.
        pending_builds.push((derivation.clone(), build_id));
        pending_paths.insert(derivation.clone());
        queue.extend(new_refs);
    } else {
        // Already in the store — record as completed; no further traversal needed.
        add_existing_build(Arc::clone(state), derivation.clone(), evaluation.id, build_id).await?;
        debug!(build_id = %build_id, path = %derivation, "Skipping — already in store");
    }

    // Record the parent → this dependency edge.
    if let Some(pid) = parent_id {
        debug!(build = %pid, dependency = %build_id, "Parent dependency edge");
        acc.dependencies.push(MBuildDependency {
            id: Uuid::new_v4(),
            build: pid,
            dependency: build_id,
        });
    }

    Ok(())
}

/// After the BFS, collects architecture and feature metadata for all pending builds
/// in parallel and pushes them into the accumulator.
async fn finalize_pending_builds(
    state: &Arc<ServerState>,
    acc: &mut EvaluationAccumulator,
    evaluation: &MEvaluation,
    pending_builds: &[(String, Uuid)],
    cloned_ids: &HashSet<Uuid>,
) -> Result<()> {
    type FeaturesResult = Result<(Uuid, String, entity::server::Architecture, Vec<String>)>;

    let handles: Vec<JoinHandle<FeaturesResult>> = pending_builds
        .iter()
        .map(|(path, build_id)| {
            let p = path.clone();
            let id = *build_id;
            tokio::task::spawn(async move {
                let (arch, features) = get_features(p.as_str())
                    .await
                    .with_context(|| format!("get_features for {}", p))?;
                Ok((id, p, arch, features))
            })
        })
        .collect();

    for handle in handles {
        let (build_id, path, arch, features) =
            handle.await.context("get_features task panicked")??;

        if cloned_ids.contains(&build_id) {
            // Already inserted into DB during BFS — skip.
            continue;
        }

        // Defer feature insertion: build_feature has a FK to build, so features can only
        // be inserted after the builds are bulk-inserted into the DB by the caller.
        acc.pending_features.push((build_id, features));

        let status = if path.ends_with(".drv") {
            BuildStatus::Created
        } else {
            BuildStatus::Completed
        };

        debug!(build_id = %build_id, path = %path, "Registering pending build");
        acc.builds.push(MBuild {
            id: build_id,
            evaluation: evaluation.id,
            derivation_path: path,
            architecture: arch,
            status,
            server: None,
            log_id: Some(build_id),
            build_time_ms: None,
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
        });
    }

    Ok(())
}

/// BFS traversal over a build's dependency graph.
///
/// All derivations in the current frontier have their `get_pathinfo` calls issued
/// concurrently via [`FuturesUnordered`]. Results are processed sequentially as they
/// arrive so deduplication structures stay consistent across a batch. Within each
/// node, store-presence checks are batched into a single `get_missing_builds` call.
///
/// New builds start as `Created` (not `Queued`) so the scheduler cannot pick them
/// up before the bulk dependency insert completes.
pub(super) async fn query_all_dependencies(
    state: Arc<ServerState>,
    acc: &mut EvaluationAccumulator,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    dependencies: Vec<String>,
) -> Result<()> {
    // Work queue: (derivation_path, parent_build_id, this_build_id)
    let mut queue: Vec<(String, Option<Uuid>, Uuid)> = dependencies
        .into_iter()
        .map(|d| (d, None, Uuid::new_v4()))
        .collect();

    let mut pending_builds: Vec<(String, Uuid)> = Vec::new();
    let mut pending_paths: HashSet<String> = HashSet::new();
    let mut cloned_ids: HashSet<Uuid> = HashSet::new();

    // Reserve 1 permit for `get_missing_builds` called inside `process_node`.
    // If the entire queue were drained at once and frontier >= pool_size, outer tasks
    // would fill the semaphore's FIFO queue with waiters. When one outer task completes
    // and releases a permit, the semaphore gives it to the next FIFO waiter — another
    // outer task — rather than to `get_missing_builds`'s inner tasks (registered later).
    // Those inner tasks then wait forever while the outer tasks sit unpolled with held
    // permits, causing an indefinite stall.  Capping the frontier at pool_size-1
    // ensures all outer tasks acquire permits immediately (no FIFO waiters), so the
    // one free permit is always available for `process_node`.
    let max_concurrent = state.cli.max_nixdaemon_connections.saturating_sub(1).max(1);

    while !queue.is_empty() {
        // Drain at most max_concurrent items to avoid pool starvation (see above).
        // FuturesUnordered is used (not tokio::spawn) so pool guards don't need Send.
        // Results arrive in completion order; deduplication state is updated
        // immediately so later results in the same batch see prior work.
        let batch_end = queue.len().min(max_concurrent);
        let mut tasks: FuturesUnordered<_> = queue
            .drain(..batch_end)
            .map(|(dep, parent_id, build_id)| {
                let state = Arc::clone(&state);
                async move {
                    let mut store = state
                        .nix_store_pool
                        .acquire()
                        .await
                        .context("acquire store for pathinfo")?;

                    let info = get_pathinfo(nix_store_path(&dep), &mut *store)
                        .await
                        .context("get_pathinfo")?
                        .with_context(|| format!("derivation not found in Nix store: {}", dep))?;
                    anyhow::Ok((dep, parent_id, build_id, info))
                }
            })
            .collect();

        while let Some(result) = tasks.next().await {
            let (derivation, parent_id, build_id, path_info) = result?;
            process_node(
                &state,
                acc,
                evaluation,
                organization_id,
                &mut queue,
                &mut pending_builds,
                &mut pending_paths,
                &mut cloned_ids,
                derivation,
                parent_id,
                build_id,
                path_info,
            )
            .await?;
        }
    }

    finalize_pending_builds(&state, acc, evaluation, &pending_builds, &cloned_ids).await?;

    Ok(())
}
