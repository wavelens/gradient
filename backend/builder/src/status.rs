/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::status::update_build_status;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

/// Propagates a status change through the dependent build graph.
///
/// Dependents of a `Failed` build are set to `Aborted` (they didn't fail themselves).
/// After all propagation is complete the original build is updated and the evaluation
/// status is re-checked.
pub(super) async fn update_build_status_recursivly(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    let evaluation_id = build.evaluation;
    let mut queue = VecDeque::new();
    let mut processed = HashSet::new();
    // Each queued entry is (build_id, derivation_id).
    queue.push_back((build.id, build.derivation));

    while let Some((current_build_id, current_derivation_id)) = queue.pop_front() {
        if !processed.insert(current_build_id) {
            continue;
        }

        // Walk reverse derivation_dependency edges: which derivations
        // depend on `current_derivation_id`?
        let reverse_edges = match EDerivationDependency::find()
            .filter(CDerivationDependency::Dependency.eq(current_derivation_id))
            .all(&state.db)
            .await
        {
            Ok(edges) => edges,
            Err(e) => {
                error!(error = %e, %current_derivation_id, "Failed to query reverse derivation_dependency");
                continue;
            }
        };

        if reverse_edges.is_empty() {
            continue;
        }

        // Map back to builds of the same evaluation.
        let dependent_derivation_ids: Vec<Uuid> =
            reverse_edges.into_iter().map(|e| e.derivation).collect();
        let mut dep_build_cond = Condition::any();
        for did in &dependent_derivation_ids {
            dep_build_cond = dep_build_cond.add(CBuild::Derivation.eq(*did));
        }

        let status_condition = if status == BuildStatus::Aborted
            || status == BuildStatus::DependencyFailed
            || status == BuildStatus::Failed
        {
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building))
        } else {
            Condition::all().add(CBuild::Status.ne(status.clone()))
        };

        let dependent_builds = match EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(dep_build_cond)
            .filter(status_condition)
            .all(&state.db)
            .await
        {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, "Failed to query dependent builds for update");
                continue;
            }
        };

        // Update dependent builds and add them to the queue for further processing.
        // Dependents of a failed build get DependencyFailed (they didn't fail themselves).
        let propagated_status =
            if status == BuildStatus::Failed || status == BuildStatus::DependencyFailed {
                BuildStatus::DependencyFailed
            } else {
                status.clone()
            };
        for dependent_build in dependent_builds {
            let dep_id = dependent_build.id;
            let dep_drv = dependent_build.derivation;
            update_build_status(
                Arc::clone(&state),
                dependent_build,
                propagated_status.clone(),
            )
            .await;
            queue.push_back((dep_id, dep_drv));
        }
    }

    // Finally update the original build with the actual status.
    let build = update_build_status(Arc::clone(&state), build, status.clone()).await;
    check_evaluation_status(state, build.evaluation).await;

    build
}

/// Determines whether an evaluation is fully finished and updates its status accordingly.
///
/// Called after each build status change to detect when all builds have reached a terminal state.
pub(super) async fn check_evaluation_status(state: Arc<ServerState>, evaluation_id: Uuid) {
    let evaluation = match EEvaluation::find_by_id(evaluation_id).one(&state.db).await {
        Ok(Some(eval)) => eval,
        Ok(None) => {
            error!(evaluation_id = %evaluation_id, "Evaluation not found for status check");
            return;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "Failed to query evaluation for status check");
            return;
        }
    };

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .all(&state.db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "Failed to query builds for evaluation status check");
            return;
        }
    };

    let statuses = builds
        .into_iter()
        .map(|b| b.status)
        .collect::<Vec<BuildStatus>>();

    let in_progress = statuses.iter().any(|s| {
        matches!(
            s,
            BuildStatus::Queued | BuildStatus::Created | BuildStatus::Building
        )
    });

    let status = if statuses
        .iter()
        .all(|s| matches!(s, BuildStatus::Completed | BuildStatus::Substituted))
    {
        EvaluationStatus::Completed
    } else if !in_progress && statuses.contains(&BuildStatus::Failed) {
        EvaluationStatus::Failed
    } else if !in_progress
        && (statuses.contains(&BuildStatus::Aborted)
            || statuses.contains(&BuildStatus::DependencyFailed))
    {
        EvaluationStatus::Aborted
    } else {
        return;
    };

    gradient_core::status::update_evaluation_status(state, evaluation, status).await;
}
