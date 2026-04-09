/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared evaluation and build status helpers.
//!
//! Extracted here so both the `evaluator` and `builder` crates can call them
//! without introducing a dependency between the two.

use chrono::Utc;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::sync::Arc;
use tracing::{debug, error};
use uuid::Uuid;

use crate::types::*;

pub async fn update_build_status(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    if status == build.status {
        return build;
    }

    if (status == BuildStatus::Aborted || status == BuildStatus::DependencyFailed)
        && (build.status == BuildStatus::Completed
            || build.status == BuildStatus::Substituted
            || build.status == BuildStatus::Failed)
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
                crate::webhooks::fire_build_webhook(webhook_state, webhook_build, webhook_status)
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

pub async fn update_evaluation_status(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> MEvaluation {
    if status == evaluation.status {
        return evaluation;
    }

    // Never step away from a terminal state. This is both a local check
    // (if the in-memory row is already terminal) and an atomic DB guard
    // via the filtered update_many below — so a concurrent abort cannot
    // be clobbered by an in-flight evaluator.
    if matches!(
        evaluation.status,
        EvaluationStatus::Aborted | EvaluationStatus::Failed | EvaluationStatus::Completed
    ) {
        return evaluation;
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, "Updating evaluation status");

    let webhook_status = status.clone();
    let now = Utc::now().naive_utc();

    let update_result = EEvaluation::update_many()
        .col_expr(
            CEvaluation::Status,
            sea_orm::sea_query::Expr::value(status.clone()),
        )
        .col_expr(
            CEvaluation::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(CEvaluation::Id.eq(evaluation.id))
        .filter(
            Condition::all()
                .add(CEvaluation::Status.ne(EvaluationStatus::Aborted))
                .add(CEvaluation::Status.ne(EvaluationStatus::Failed))
                .add(CEvaluation::Status.ne(EvaluationStatus::Completed)),
        )
        .exec(&state.db)
        .await;

    match update_result {
        Ok(res) if res.rows_affected == 0 => {
            // Row was concurrently transitioned to a terminal state —
            // honor it and return the fresh value instead of clobbering.
            return EEvaluation::find_by_id(evaluation.id)
                .one(&state.db)
                .await
                .ok()
                .flatten()
                .unwrap_or(evaluation);
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to update evaluation status");
            return evaluation;
        }
        Ok(_) => {}
    }

    let updated_eval = EEvaluation::find_by_id(evaluation.id)
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let mut e = evaluation.clone();
            e.status = status;
            e.updated_at = now;
            e
        });

    let webhook_state = Arc::clone(&state);
    let webhook_eval = updated_eval.clone();
    tokio::spawn(async move {
        crate::webhooks::fire_evaluation_webhook(webhook_state, webhook_eval, webhook_status).await;
    });
    updated_eval
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
    // If the evaluation is already in a terminal state (e.g. it was
    // aborted while we were running), don't record a spurious error or
    // overwrite the status — just return the current row.
    if matches!(
        evaluation.status,
        EvaluationStatus::Aborted | EvaluationStatus::Failed | EvaluationStatus::Completed
    ) {
        return evaluation;
    }

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
