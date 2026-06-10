/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{PHASE_SUBJECT_EVALUATION, record_phase_event};
use crate::DbContext;
use crate::state_machine::EvalStateMachine;
use gradient_types::*;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{Condition, ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, error, warn};

pub async fn update_evaluation_status(
    ctx: &DbContext,
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

    let event_status = status;
    let now = gradient_types::now();

    let mut update = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, sea_orm::sea_query::Expr::value(status))
        .col_expr(CEvaluation::UpdatedAt, sea_orm::sea_query::Expr::value(now));

    if !matches!(status, EvaluationStatus::Waiting) {
        update = update.col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(Option::<serde_json::Value>::None),
        );
    }

    let phase_col = match status {
        EvaluationStatus::Fetching => Some(CEvaluation::FetchStartedAt),
        EvaluationStatus::EvaluatingFlake => Some(CEvaluation::EvalFlakeStartedAt),
        EvaluationStatus::EvaluatingDerivation => Some(CEvaluation::EvalDrvStartedAt),
        EvaluationStatus::Building => Some(CEvaluation::BuildingStartedAt),
        EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted => {
            Some(CEvaluation::FinishedAt)
        }
        _ => None,
    };
    if let Some(col) = phase_col {
        update = update.col_expr(col, sea_orm::sea_query::Expr::value(now));
    }

    let update_result = update
        .filter(CEvaluation::Id.eq(evaluation.id))
        .filter(
            Condition::all()
                .add(CEvaluation::Status.ne(EvaluationStatus::Aborted))
                .add(CEvaluation::Status.ne(EvaluationStatus::Failed))
                .add(CEvaluation::Status.ne(EvaluationStatus::Completed)),
        )
        .exec(&ctx.worker_db)
        .await;

    match update_result {
        Ok(res) if res.rows_affected == 0 => {
            // Row was concurrently transitioned to a terminal state -
            // honor it and return the fresh value instead of clobbering.
            return EEvaluation::find_by_id(evaluation.id)
                .one(&ctx.worker_db)
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
        .one(&ctx.worker_db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let mut e = evaluation.clone();
            e.status = status;
            e.updated_at = now;
            e
        });

    let _ = ctx
        .board_events
        .send(gradient_types::BoardEvent::EvaluationStatusChanged {
            project: updated_eval.project.map(|p| p.into_inner()),
            evaluation_id: updated_eval.id.into_inner(),
            status: i32::from(event_status) as i16,
        });

    let action_ctx = ctx.clone();
    let action_eval = updated_eval.clone();
    ctx.shutdown.spawn(async move {
        action_ctx
            .reactor
            .on_eval_terminal(&action_ctx, action_eval, event_status)
            .await;
    });

    let pe_ctx = ctx.clone();
    let pe_id = updated_eval.id.into_inner();
    ctx.shutdown.spawn(async move {
        record_phase_event(
            &pe_ctx.worker_db,
            PHASE_SUBJECT_EVALUATION,
            pe_id,
            i32::from(event_status) as i16,
            None,
            now,
        )
        .await;
    });

    updated_eval
}

/// Records an error-level `evaluation_message` row and transitions the evaluation status.
///
/// `source` identifies where the error originated - e.g. `"flake-prefetch"`,
/// `"nix-eval"`, `"nix-eval:packages.x86_64-linux.hello"`, `"db-insert"`.
pub async fn update_evaluation_status_with_error(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    error_message: String,
    source: Option<String>,
) -> MEvaluation {
    // If the evaluation is already in a terminal state (e.g. it was
    // aborted while we were running), don't record a spurious error or
    // overwrite the status - just return the current row.
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
        created_at: Set(gradient_types::now()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&ctx.worker_db).await {
        error!(error = %e, evaluation_id = %evaluation.id, "Failed to insert evaluation_message");
    }

    update_evaluation_status(ctx, evaluation, status).await
}
