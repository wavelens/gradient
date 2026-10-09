/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{PhaseSubjectKind, record_phase_event};
use crate::DbContext;
use crate::state_machine::EvalStateMachine;
use chrono::NaiveDateTime;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
};
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
        // A concurrent writer moved the row to a terminal state; keep its value.
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

    report_evaluation_status(ctx, &updated_eval, event_status, now).await?;
    Ok(updated_eval)
}

fn reopen_evaluation_sql() -> String {
    format!(
        "UPDATE evaluation e \
         SET status = {building}, finished_at = NULL, waiting_reason = NULL, \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE e.id = $1 AND e.status IN ({reopenable}) \
           AND NOT EXISTS (\
             SELECT 1 FROM evaluation o \
             WHERE o.task = e.task AND o.id <> e.id \
               AND (o.created_at > e.created_at OR o.status NOT IN ({terminal})))",
        building = crate::sql::status::eval(EvaluationStatus::Building),
        reopenable = crate::sql::status::eval_in(&EvaluationStatus::REOPENABLE),
        terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

crate::sql_fn! {
    REOPEN_EVALUATION = reopen_evaluation_sql,
        params = [EvaluationId];
}

pub async fn reopen_evaluation(ctx: &DbContext, evaluation: &MEvaluation) -> Result<bool, DbErr> {
    let reopened = ctx
        .worker_db
        .execute_raw(REOPEN_EVALUATION.bind([evaluation.id.into_inner().into()]))
        .await?;
    if reopened.rows_affected() == 0 {
        return Ok(false);
    }

    report_evaluation_status(
        ctx,
        evaluation,
        EvaluationStatus::Building,
        gradient_types::now(),
    )
    .await?;
    Ok(true)
}

async fn report_evaluation_status(
    ctx: &DbContext,
    evaluation: &MEvaluation,
    status: EvaluationStatus,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    crate::deliveries::events::record(
        &ctx.worker_db,
        &ctx.events,
        gradient_types::events::evaluation::Reported {
            evaluation_id: evaluation.id,
            phase: gradient_types::events::evaluation::Phase::of_status(status),
            status: i32::from(status) as i16,
            task: evaluation.task,
            ..Default::default()
        },
    )
    .await?;
    ctx.delivery_wake.notify_one();

    record_phase_event(
        &ctx.worker_db,
        PhaseSubjectKind::Evaluation,
        evaluation.id.into_inner(),
        i32::from(status) as i16,
        None,
        now,
    )
    .await
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
