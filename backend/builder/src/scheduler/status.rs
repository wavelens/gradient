/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tracing::{debug, error};
use uuid::Uuid;

pub async fn update_build_status(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    if status == build.status {
        return build;
    }

    if (status == BuildStatus::Aborted || status == BuildStatus::DependencyFailed)
        && (build.status == BuildStatus::Completed || build.status == BuildStatus::Failed)
    {
        return build;
    }

    debug!(build_id = %build.id, status = ?status, "Updating build status");

    let mut active_build: ABuild = build.clone().into_active_model();

    let webhook_status = status.clone();
    active_build.status = Set(status);
    active_build.updated_at = Set(Utc::now().naive_utc());

    match active_build.update(&state.db).await {
        Ok(updated_build) => {
            let webhook_state = Arc::clone(&state);
            let webhook_build = updated_build.clone();
            tokio::spawn(async move {
                gradient_core::webhooks::fire_build_webhook(
                    webhook_state,
                    webhook_build,
                    webhook_status,
                )
                .await;
            });

            // Finalize the build log on terminal state transitions so backends
            // like S3 can upload the local file to remote storage.
            if matches!(
                updated_build.status,
                BuildStatus::Completed
                    | BuildStatus::Failed
                    | BuildStatus::Aborted
                    | BuildStatus::DependencyFailed
            ) {
                let log_state = Arc::clone(&state);
                let log_id = updated_build.log_id.unwrap_or(updated_build.id);
                tokio::spawn(async move {
                    if let Err(e) = log_state.log_storage.finalize(log_id).await {
                        error!(error = %e, build_id = %log_id, "Failed to finalize build log");
                    }
                });
            }

            updated_build
        }
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to update build status");
            build
        }
    }
}

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

pub async fn update_evaluation_status(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> MEvaluation {
    if status == evaluation.status {
        return evaluation;
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, "Updating evaluation status");

    let mut active_evaluation: AEvaluation = evaluation.clone().into_active_model();

    let webhook_status = status.clone();
    active_evaluation.status = Set(status);
    active_evaluation.updated_at = Set(Utc::now().naive_utc());

    match active_evaluation.update(&state.db).await {
        Ok(updated_eval) => {
            let webhook_state = Arc::clone(&state);
            let webhook_eval = updated_eval.clone();
            tokio::spawn(async move {
                gradient_core::webhooks::fire_evaluation_webhook(
                    webhook_state,
                    webhook_eval,
                    webhook_status,
                )
                .await;
            });
            updated_eval
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to update evaluation status");
            evaluation
        }
    }
}

/// Records an error-level `evaluation_message` row and transitions the evaluation status.
///
/// `source` identifies where the error originated — e.g. `"flake-prefetch"`,
/// `"nix-eval"`, `"nix-eval:packages.x86_64-linux.hello"`, `"db-insert"`.
pub async fn update_evaluation_status_with_error(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    error_message: String,
    source: Option<String>,
) -> MEvaluation {
    debug!(evaluation_id = %evaluation.id, status = ?status, error = %error_message, ?source, "Updating evaluation status with error");

    let msg = AEvaluationMessage {
        id: Set(Uuid::new_v4()),
        evaluation: Set(evaluation.id),
        level: Set(MessageLevel::Error),
        message: Set(error_message),
        source: Set(source),
        created_at: Set(Utc::now().naive_utc()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&state.db).await {
        error!(error = %e, evaluation_id = %evaluation.id, "Failed to insert evaluation_message");
    }

    update_evaluation_status(state, evaluation, status).await
}

/// Inserts a single `evaluation_message` row without changing the evaluation status.
///
/// Use for partial failures (e.g. one attr path failed to evaluate) where the
/// evaluation as a whole continues.
pub async fn record_evaluation_message(
    state: &Arc<ServerState>,
    evaluation_id: Uuid,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) {
    let msg = AEvaluationMessage {
        id: Set(Uuid::new_v4()),
        evaluation: Set(evaluation_id),
        level: Set(level),
        message: Set(message),
        source: Set(source),
        created_at: Set(Utc::now().naive_utc()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&state.db).await {
        error!(error = %e, %evaluation_id, "Failed to insert evaluation_message");
    }
}

pub async fn abort_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    if evaluation.status == EvaluationStatus::Completed {
        return;
    }

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building)),
        )
        .all(&state.db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to query builds for evaluation abort");
            return;
        }
    };

    for build in builds {
        update_build_status(Arc::clone(&state), build, BuildStatus::Aborted).await;
    }

    update_evaluation_status(state, evaluation, EvaluationStatus::Aborted).await;
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

    update_evaluation_status(state, evaluation, status).await;
}
