/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{PhaseSubjectKind, record_phase_event};
use crate::DbContext;
use crate::state_machine::EvalStateMachine;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{debug, warn};

pub async fn update_evaluation_status(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> Result<MEvaluation, DbErr> {
    // The filtered `update_many` below is guarding atomically in the database.
    // A concurrent abort cannot be clobbered by an in-flight evaluator.
    match EvalStateMachine::validate(evaluation.status, status) {
        Ok(_) => {}
        Err(e) => {
            warn!(evaluation_id = %evaluation.id, error = %e, "Skipping invalid evaluation status transition");
            return Ok(evaluation);
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

    let updated = update
        .filter(CEvaluation::Id.eq(evaluation.id))
        .filter(
            Condition::all()
                .add(CEvaluation::Status.ne(EvaluationStatus::Aborted))
                .add(CEvaluation::Status.ne(EvaluationStatus::Failed))
                .add(CEvaluation::Status.ne(EvaluationStatus::Completed)),
        )
        .exec(&ctx.worker_db)
        .await?;

    if updated.rows_affected == 0 {
        // A concurrent writer moved the row to a terminal state, and its value must win.
        return Ok(EEvaluation::find_by_id(evaluation.id)
            .one(&ctx.worker_db)
            .await?
            .unwrap_or(evaluation));
    }

    let updated_eval = EEvaluation::find_by_id(evaluation.id)
        .one(&ctx.worker_db)
        .await?
        .unwrap_or_else(|| {
            let mut e = evaluation.clone();
            e.status = status;
            e.updated_at = now;
            e
        });

    crate::deliveries::events::record(
        &ctx.worker_db,
        &ctx.events,
        gradient_types::events::evaluation::Reported {
            evaluation_id: updated_eval.id,
            phase: gradient_types::events::evaluation::Phase::of_status(event_status),
            status: i32::from(event_status) as i16,
            task: updated_eval.task,
            ..Default::default()
        },
    )
    .await?;
    ctx.delivery_wake.notify_one();

    record_phase_event(
        &ctx.worker_db,
        PhaseSubjectKind::Evaluation,
        updated_eval.id.into_inner(),
        i32::from(event_status) as i16,
        None,
        now,
    )
    .await?;

    Ok(updated_eval)
}

pub async fn update_evaluation_status_with_error(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    error_message: String,
    source: Option<String>,
) -> Result<MEvaluation, DbErr> {
    if matches!(
        evaluation.status,
        EvaluationStatus::Aborted | EvaluationStatus::Failed | EvaluationStatus::Completed
    ) {
        return Ok(evaluation);
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, error = %error_message, ?source, "Updating evaluation status with error");

    let msg = MEvaluationMessage {
        id: EvaluationMessageId::now_v7(),
        evaluation: evaluation.id,
        level: MessageLevel::Error,
        message: error_message,
        source,
        created_at: gradient_types::now(),
    }
    .into_active_model();

    EEvaluationMessage::insert(msg).exec(&ctx.worker_db).await?;

    update_evaluation_status(ctx, evaluation, status).await
}
