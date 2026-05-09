/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared evaluation and build status helpers.
//!
//! Extracted here so both the `evaluator` and `builder` crates can call them
//! without introducing a dependency between the two.

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::state_machine::{BuildStateMachine, EvalStateMachine};
use crate::types::*;

pub async fn update_build_status(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    if build.status == status {
        return build;
    }

    match BuildStateMachine::validate(build.status, status) {
        Ok(_) => {}
        Err(e) => {
            // Loud: a rejected transition usually means the build is stuck
            // in a state the next event can't legally move it from — e.g.
            // a JobFailed arriving while the build is still `Queued`
            // because the worker's `Building` JobUpdate was lost / never
            // sent. Without this we'd silently drop the failure and the UI
            // would show the build hanging in `Queued` / `Building` forever.
            error!(
                build_id = %build.id,
                from = ?build.status,
                to = ?status,
                error = %e,
                "Skipping invalid build status transition — investigate: status update lost or out of order"
            );
            return build;
        }
    }

    info!(build_id = %build.id, from = ?build.status, to = ?status, "build status transition");

    let mut active_build: ABuild = build.clone().into_active_model();

    let webhook_status = status;
    let now = crate::types::now();
    // When transitioning out of `Building` into a terminal state, record the
    // elapsed wall-clock time. `build.updated_at` is the timestamp of the
    // previous transition (into `Building` by `Scheduler::handle_build_status_update`).
    if build.status == BuildStatus::Building
        && matches!(
            status,
            BuildStatus::Completed
                | BuildStatus::Failed
                | BuildStatus::Aborted
                | BuildStatus::DependencyFailed
        )
        && build.build_time_ms.is_none()
    {
        let elapsed_ms = (now - build.updated_at).num_milliseconds().max(0);
        active_build.build_time_ms = Set(Some(elapsed_ms));
    }
    active_build.status = Set(status);
    active_build.updated_at = Set(now);

    match active_build.update(&state.worker_db).await {
        Ok(updated_build) => {
            let webhook_state = Arc::clone(&state);
            let webhook_build = updated_build.clone();
            state.shutdown.spawn(async move {
                crate::ci::fire_build_webhook(webhook_state, webhook_build, webhook_status).await;
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
                state.shutdown.spawn(async move {
                    if let Err(e) = log_state.log_storage.finalize(log_id).await {
                        error!(error = %e, build_id = %log_id, "Failed to finalize build log");
                    }
                });
            }

            if let Some(ci_status) = crate::ci::ci_status_for_build(&updated_build.status) {
                let ci_state = Arc::clone(&state);
                let ci_build = updated_build.clone();
                state.shutdown.spawn(async move {
                    crate::ci::report_build_ci(ci_state, ci_build, ci_status).await;
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
    // The state machine validates the transition locally. The filtered update_many
    // below also guards atomically in the DB, so concurrent aborts cannot be
    // clobbered by an in-flight evaluator.
    match EvalStateMachine::validate(evaluation.status, status) {
        Ok(_) => {}
        Err(e) => {
            warn!(evaluation_id = %evaluation.id, error = %e, "Skipping invalid evaluation status transition");
            return evaluation;
        }
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, "Updating evaluation status");

    let webhook_status = status;
    let now = crate::types::now();

    let mut update = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, sea_orm::sea_query::Expr::value(status))
        .col_expr(CEvaluation::UpdatedAt, sea_orm::sea_query::Expr::value(now));

    if !matches!(status, EvaluationStatus::Waiting) {
        update = update.col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(Option::<serde_json::Value>::None),
        );
    }

    let update_result = update
        .filter(CEvaluation::Id.eq(evaluation.id))
        .filter(
            Condition::all()
                .add(CEvaluation::Status.ne(EvaluationStatus::Aborted))
                .add(CEvaluation::Status.ne(EvaluationStatus::Failed))
                .add(CEvaluation::Status.ne(EvaluationStatus::Completed)),
        )
        .exec(&state.worker_db)
        .await;

    match update_result {
        Ok(res) if res.rows_affected == 0 => {
            // Row was concurrently transitioned to a terminal state —
            // honor it and return the fresh value instead of clobbering.
            return EEvaluation::find_by_id(evaluation.id)
                .one(&state.worker_db)
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
        .one(&state.worker_db)
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
    state.shutdown.spawn(async move {
        crate::ci::fire_evaluation_webhook(webhook_state, webhook_eval, webhook_status).await;
    });

    if let Some(ci_status) = crate::ci::ci_status_for_evaluation(&updated_eval.status) {
        let ci_state = Arc::clone(&state);
        let ci_eval = updated_eval.clone();
        state.shutdown.spawn(async move {
            crate::ci::report_evaluation_ci(ci_state, ci_eval, ci_status).await;
        });
    }

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
        id: Set(EvaluationMessageId::now_v7()),
        evaluation: Set(evaluation.id),
        level: Set(MessageLevel::Error),
        message: Set(error_message),
        source: Set(source),
        created_at: Set(crate::types::now()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&state.worker_db).await {
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
    evaluation_id: EvaluationId,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) {
    let msg = AEvaluationMessage {
        id: Set(EvaluationMessageId::now_v7()),
        evaluation: Set(evaluation_id),
        level: Set(level),
        message: Set(message),
        source: Set(source),
        created_at: Set(crate::types::now()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&state.worker_db).await {
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
        .all(&state.worker_db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to query builds for evaluation abort");
            return;
        }
    };

    for build in builds {
        if build.via.is_some() {
            // Follower: aborting it does not interrupt the leader's work in
            // another evaluation. Clear `via` so the eventual leader-completion
            // sweep skips it, then mark Aborted.
            abort_follower(&state, build).await;
            continue;
        }

        // Leader (or plain build).
        let has_followers = match EBuild::find()
            .filter(CBuild::Via.eq(build.id))
            .one(&state.worker_db)
            .await
        {
            Ok(opt) => opt.is_some(),
            Err(e) => {
                error!(error = %e, build_id = %build.id, "Failed to query followers for abort");
                false
            }
        };

        if has_followers && build.status == BuildStatus::Building {
            // Already running on a worker — let it finish so followers in
            // other (non-aborted) evaluations get the result.
            continue;
        }

        if has_followers && matches!(build.status, BuildStatus::Queued | BuildStatus::Created) {
            // Hand off leadership before aborting.
            if let Err(e) = reelect_leader(&state, &build).await {
                error!(error = %e, build_id = %build.id, "Failed to re-elect leader on abort");
            }
        }

        update_build_status(Arc::clone(&state), build, BuildStatus::Aborted).await;
    }

    update_evaluation_status(state, evaluation, EvaluationStatus::Aborted).await;
}

async fn abort_follower(state: &Arc<ServerState>, build: MBuild) {
    let mut active: ABuild = build.clone().into_active_model();
    active.via = Set(None);
    if let Err(e) = active.update(&state.worker_db).await {
        error!(error = %e, build_id = %build.id, "Failed to clear via on follower abort");
        return;
    }
    let reloaded = match EBuild::find_by_id(build.id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to reload follower for abort");
            return;
        }
    };
    update_build_status(Arc::clone(state), reloaded, BuildStatus::Aborted).await;
}

/// Promote one follower of `leader` to be the new leader, then re-point any
/// remaining followers at the new leader's id. Picks the oldest follower by
/// `created_at` for stability. No-op if no followers exist.
async fn reelect_leader(state: &Arc<ServerState>, leader: &MBuild) -> Result<(), sea_orm::DbErr> {
    use sea_orm::QueryOrder;

    let new_leader = EBuild::find()
        .filter(CBuild::Via.eq(leader.id))
        .order_by_asc(CBuild::CreatedAt)
        .one(&state.worker_db)
        .await?;
    let Some(new_leader) = new_leader else {
        return Ok(());
    };

    let mut active: ABuild = new_leader.clone().into_active_model();
    active.via = Set(None);
    active.update(&state.worker_db).await?;

    EBuild::update_many()
        .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(new_leader.id))
        .filter(CBuild::Via.eq(leader.id))
        .exec(&state.worker_db)
        .await?;

    debug!(
        old_leader = %leader.id,
        new_leader = %new_leader.id,
        "re-elected build leader"
    );
    Ok(())
}
