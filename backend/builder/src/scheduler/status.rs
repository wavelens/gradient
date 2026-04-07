/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use gradient_core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter};
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
                gradient_core::webhooks::fire_build_webhook(webhook_state, webhook_build, webhook_status)
                    .await;
            });
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
    let mut queue = VecDeque::new();
    let mut processed = HashSet::new();
    queue.push_back(build.id);

    while let Some(current_build_id) = queue.pop_front() {
        if processed.contains(&current_build_id) {
            continue;
        }
        processed.insert(current_build_id);

        let dependencies = match EBuildDependency::find()
            .filter(CBuildDependency::Dependency.eq(current_build_id))
            .all(&state.db)
            .await
        {
            Ok(deps) => deps.into_iter().map(|d| d.build).collect::<Vec<Uuid>>(),
            Err(e) => {
                error!(error = %e, build_id = %current_build_id, "Failed to query build dependencies for update");
                continue;
            }
        };

        if dependencies.is_empty() {
            continue;
        }

        let mut condition = Condition::any();
        for dependency in &dependencies {
            condition = condition.add(CBuild::Id.eq(*dependency));
        }

        let status_condition = if status == BuildStatus::Aborted || status == BuildStatus::DependencyFailed || status == BuildStatus::Failed {
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building))
        } else {
            Condition::all().add(CBuild::Status.ne(status.clone()))
        };

        let dependent_builds = match EBuild::find()
            .filter(condition)
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
        let propagated_status = if status == BuildStatus::Failed || status == BuildStatus::DependencyFailed {
            BuildStatus::DependencyFailed
        } else {
            status.clone()
        };
        for dependent_build in dependent_builds {
            update_build_status(
                Arc::clone(&state),
                dependent_build.clone(),
                propagated_status.clone(),
            )
            .await;
            queue.push_back(dependent_build.id);
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

pub async fn update_evaluation_status_with_error(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    error_message: String,
) -> MEvaluation {
    if status == evaluation.status && evaluation.error.as_ref() == Some(&error_message) {
        return evaluation;
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, error = %error_message, "Updating evaluation status with error");

    let mut active_evaluation: AEvaluation = evaluation.clone().into_active_model();
    active_evaluation.status = Set(status);
    active_evaluation.error = Set(Some(error_message));
    active_evaluation.updated_at = Set(Utc::now().naive_utc());

    match active_evaluation.update(&state.db).await {
        Ok(updated_eval) => updated_eval,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to update evaluation status with error");
            evaluation
        }
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

    let status = if statuses.iter().all(|s| *s == BuildStatus::Completed) {
        EvaluationStatus::Completed
    } else if !in_progress && statuses.contains(&BuildStatus::Failed) {
        EvaluationStatus::Failed
    } else if !in_progress && (statuses.contains(&BuildStatus::Aborted) || statuses.contains(&BuildStatus::DependencyFailed)) {
        EvaluationStatus::Aborted
    } else {
        return;
    };

    update_evaluation_status(state, evaluation, status).await;
}
