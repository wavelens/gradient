/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `BuildOutput` messages from workers and build job lifecycle.

use std::sync::Arc;

use anyhow::{Context, Result};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::db::{update_build_status, update_evaluation_status};
use gradient_core::types::*;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::jobs::PendingBuildJob;
use gradient_core::types::proto::BuildOutput;

pub async fn handle_build_output(
    state: &Arc<ServerState>,
    _job: &PendingBuildJob,
    build_id: Uuid,
    outputs: Vec<BuildOutput>,
) -> Result<()> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await
        .context("fetch build")?
        .with_context(|| format!("build {} not found", build_id))?;

    let derivation_id = build.derivation;

    for output in &outputs {
        let existing = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(derivation_id))
            .filter(CDerivationOutput::Name.eq(&output.name))
            .one(&state.db)
            .await
            .context("fetch derivation_output")?;

        if let Some(row) = existing {
            let mut active = row.into_active_model();
            if let Some(nar_size) = output.nar_size {
                active.nar_size = Set(Some(nar_size));
            }
            if let Some(ref nar_hash) = output.nar_hash {
                active.file_hash = Set(Some(nar_hash.clone()));
            }
            active.has_artefacts = Set(output.has_artefacts);
            if let Err(e) = active.update(&state.db).await {
                error!(error = %e, %build_id, output_name = %output.name, "failed to update derivation_output");
            }
        } else {
            warn!(%build_id, output_name = %output.name, "derivation_output row not found");
        }
    }

    info!(%build_id, output_count = outputs.len(), "build outputs recorded");
    Ok(())
}

/// Records NAR metadata (size and hash) on the `derivation_output` row matching
/// `store_path`.  Called when the server receives `ClientMessage::NarReady` after
/// a worker finishes compressing and uploading a build output.
pub async fn handle_nar_ready(
    state: &Arc<ServerState>,
    store_path: &str,
    nar_size: u64,
    nar_hash: &str,
) -> Result<()> {
    let existing = EDerivationOutput::find()
        .filter(CDerivationOutput::Output.eq(store_path))
        .one(&state.db)
        .await
        .context("fetch derivation_output by store_path")?;

    if let Some(row) = existing {
        let mut active = row.into_active_model();
        active.nar_size = Set(Some(nar_size as i64));
        active.file_hash = Set(Some(nar_hash.to_string()));
        if let Err(e) = active.update(&state.db).await {
            error!(store_path, error = %e, "failed to update derivation_output from NarReady");
        } else {
            info!(store_path, nar_size, "NarReady recorded");
        }
    } else {
        debug!(
            store_path,
            "NarReady for store path not in derivation_output (expected for source paths)"
        );
    }

    Ok(())
}

pub async fn handle_build_job_completed(state: &Arc<ServerState>, build_id: Uuid) -> Result<()> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await? {
        Some(b) => b,
        None => {
            warn!(%build_id, "build not found on job_completed");
            return Ok(());
        }
    };
    let evaluation_id = build.evaluation;
    update_build_status(Arc::clone(state), build, BuildStatus::Completed).await;
    check_evaluation_done(state, evaluation_id).await
}

pub async fn handle_build_job_failed(
    state: &Arc<ServerState>,
    build_id: Uuid,
    _error: &str,
) -> Result<()> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await? {
        Some(b) => b,
        None => {
            warn!(%build_id, "build not found on job_failed");
            return Ok(());
        }
    };
    let evaluation_id = build.evaluation;
    let derivation_id = build.derivation;
    update_build_status(Arc::clone(state), build, BuildStatus::Failed).await;
    cascade_dependency_failed(state, evaluation_id, derivation_id).await?;
    check_evaluation_done(state, evaluation_id).await
}

async fn cascade_dependency_failed(
    state: &Arc<ServerState>,
    evaluation_id: Uuid,
    failed_derivation_id: Uuid,
) -> Result<()> {
    // BFS: walk the derivation_dependency graph and mark all transitive
    // dependents as DependencyFailed.
    let mut queue = vec![failed_derivation_id];

    while let Some(failed_drv) = queue.pop() {
        let dependents = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.is_in(vec![BuildStatus::Created, BuildStatus::Queued]))
            .all(&state.db)
            .await
            .context("fetch builds for cascade")?;

        for build in dependents {
            let dep_edge = EDerivationDependency::find()
                .filter(CDerivationDependency::Derivation.eq(build.derivation))
                .filter(CDerivationDependency::Dependency.eq(failed_drv))
                .one(&state.db)
                .await?;
            if dep_edge.is_some() {
                let cascaded_drv = build.derivation;
                update_build_status(Arc::clone(state), build, BuildStatus::DependencyFailed).await;
                queue.push(cascaded_drv);
            }
        }
    }
    Ok(())
}

/// Transitions the evaluation to its final state if all builds are done.
///
/// Returns early if any build is still active (Created/Queued/Building) or if
/// the evaluation is not in `Building` state. Otherwise sets `Failed` when at
/// least one build failed (Failed or DependencyFailed), else `Completed`.
pub(crate) async fn check_evaluation_done(
    state: &Arc<ServerState>,
    evaluation_id: Uuid,
) -> Result<()> {
    let active = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.is_in(vec![
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(&state.db)
        .await
        .context("fetch active builds")?;

    if !active.is_empty() {
        return Ok(());
    }

    let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
    else {
        return Ok(());
    };

    if !matches!(eval.status, EvaluationStatus::Building) {
        return Ok(());
    }

    let failed = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Status.is_in(vec![BuildStatus::Failed, BuildStatus::DependencyFailed]))
        .all(&state.db)
        .await
        .context("fetch failed builds")?;

    let target = if failed.is_empty() {
        EvaluationStatus::Completed
    } else {
        EvaluationStatus::Failed
    };
    info!(%evaluation_id, ?target, "evaluation finished");
    update_evaluation_status(Arc::clone(state), eval, target).await;
    Ok(())
}

/// Sweep every in-flight evaluation (`Building` or `Waiting`) and reconcile
/// its status against the current set of connected workers' capabilities.
///
/// - `Building` → `Waiting` when **none** of the eval's still-pending builds
///   has a connected worker whose `architectures` + `system_features` can
///   satisfy it. Surfaces "no worker configured for these builds" in the UI
///   instead of leaving the eval stuck silently.
/// - `Waiting` → `Building` when **any** pending build now has a matching
///   worker (e.g. an aarch64 worker just connected, or an existing worker
///   added a new system feature via re-advertised capabilities).
///
/// Cheap: one query for the small set of in-flight evals, one query for
/// their non-terminal builds + derivations, one query for required features.
/// Worker caps are taken as a snapshot from the in-memory pool. Safe to call
/// from the dispatch loop and from worker-capability change hooks.
pub async fn reconcile_waiting_state(
    state: &Arc<ServerState>,
    worker_caps: &[(Vec<String>, Vec<String>)],
) -> Result<()> {
    use std::collections::HashMap;

    let evals = EEvaluation::find()
        .filter(
            CEvaluation::Status
                .is_in(vec![EvaluationStatus::Building, EvaluationStatus::Waiting]),
        )
        .all(&state.db)
        .await
        .context("fetch in-flight evaluations")?;
    if evals.is_empty() {
        return Ok(());
    }

    for eval in evals {
        let pending_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(eval.id))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .all(&state.db)
            .await
            .context("fetch pending builds")?;

        if pending_builds.is_empty() {
            // Nothing left to gate — terminal-status decision happens in
            // `check_evaluation_done`, not here.
            continue;
        }

        // Load the derivations these builds reference so we know each one's
        // target architecture, then look up their required features.
        let drv_ids: Vec<Uuid> = pending_builds.iter().map(|b| b.derivation).collect();
        let drvs = EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids.clone()))
            .all(&state.db)
            .await
            .context("fetch derivations for pending builds")?;
        let drv_by_id: HashMap<Uuid, MDerivation> =
            drvs.into_iter().map(|d| (d.id, d)).collect();

        // Required features: derivation_feature → feature.name.
        let edges = EDerivationFeature::find()
            .filter(CDerivationFeature::Derivation.is_in(drv_ids.clone()))
            .all(&state.db)
            .await
            .context("fetch derivation_feature edges")?;
        let mut features_by_drv: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for e in &edges {
            features_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.feature);
        }
        let feature_ids: Vec<Uuid> = edges.iter().map(|e| e.feature).collect();
        let feature_rows = if feature_ids.is_empty() {
            vec![]
        } else {
            EFeature::find()
                .filter(CFeature::Id.is_in(feature_ids))
                .all(&state.db)
                .await
                .context("fetch feature names")?
        };
        let feature_name: HashMap<Uuid, String> =
            feature_rows.into_iter().map(|f| (f.id, f.name)).collect();

        // For each pending build, ask: does any connected worker satisfy
        // (build's arch ∈ worker.architectures) ∧ (every required feature ∈
        // worker.system_features)?
        let any_buildable = pending_builds.iter().any(|b| {
            let drv = match drv_by_id.get(&b.derivation) {
                Some(d) => d,
                None => return false,
            };
            let required: Vec<&str> = features_by_drv
                .get(&b.derivation)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|i| feature_name.get(i).map(String::as_str))
                        .collect()
                })
                .unwrap_or_default();
            worker_caps.iter().any(|(arch, feats)| {
                let arch_ok = drv.architecture == "builtin"
                    || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required
                    .iter()
                    .all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            })
        });

        let target = if any_buildable {
            EvaluationStatus::Building
        } else {
            EvaluationStatus::Waiting
        };
        if eval.status != target {
            info!(
                evaluation_id = %eval.id,
                from = ?eval.status,
                to = ?target,
                pending = pending_builds.len(),
                workers = worker_caps.len(),
                "reconciling evaluation waiting state"
            );
            update_evaluation_status(Arc::clone(state), eval, target).await;
        }
    }

    Ok(())
}
