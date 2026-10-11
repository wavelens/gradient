/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_message::EvaluationFailure;
use super::logging::{PhaseSubjectKind, record_phase_event};
use crate::DbContext;
use crate::sql::StatusWrite;
use chrono::NaiveDateTime;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::transition::Transition;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter, UpdateMany,
};
use tracing::{debug, warn};

pub async fn update_evaluation_status(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> Result<MEvaluation, DbErr> {
    if evaluation.status != status && !status.sources().contains(&evaluation.status) {
        warn!(evaluation_id = %evaluation.id, from = ?evaluation.status, to = ?status, "Skipping invalid evaluation status transition");
        return Ok(evaluation);
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
        .filter(CEvaluation::Status.is_in(sources_in_the_row(status)))
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

pub fn evaluation_change(transition: Transition<EvaluationStatus>) -> UpdateMany<EEvaluation> {
    EEvaluation::update_many()
        .col_expr(
            CEvaluation::Status,
            sea_orm::sea_query::Expr::value(transition.to),
        )
        .filter(CEvaluation::Status.is_in(transition.from.iter().copied()))
}

pub async fn change_evaluation_status<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    transition: Transition<EvaluationStatus>,
    columns: impl FnOnce(UpdateMany<EEvaluation>) -> UpdateMany<EEvaluation>,
) -> Result<Option<MEvaluation>, DbErr> {
    let change = evaluation_change(transition).col_expr(
        CEvaluation::UpdatedAt,
        sea_orm::sea_query::Expr::value(gradient_types::now()),
    );
    let changed = columns(change)
        .filter(CEvaluation::Id.eq(evaluation))
        .exec_with_returning(db)
        .await?;
    Ok(changed.into_iter().next())
}

fn sources_in_the_row(status: EvaluationStatus) -> Vec<EvaluationStatus> {
    let refresh = (!status.is_terminal()).then_some(status);
    status.sources().iter().copied().chain(refresh).collect()
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
        building = crate::sql::status::to(EvaluationStatus::REOPEN),
        reopenable = crate::sql::status::from(EvaluationStatus::REOPEN),
        terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

crate::sql_fn! {
    REOPEN_EVALUATION = reopen_evaluation_sql,
        params = [EvaluationId],
        status_write = StatusWrite::evaluation(EvaluationStatus::REOPEN);
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
            phase: gradient_types::events::evaluation::Phase::from(status),
            status: i16::from(status),
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
        i16::from(status),
        None,
        now,
    )
    .await
}

pub async fn update_evaluation_status_with_error(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    failure: &EvaluationFailure<'_>,
) -> Result<MEvaluation, DbErr> {
    if evaluation.status.is_terminal() {
        return Ok(evaluation);
    }

    let source = failure.source();
    debug!(evaluation_id = %evaluation.id, status = ?status, error = %failure, source, "Updating evaluation status with error");

    let msg = MEvaluationMessage {
        id: EvaluationMessageId::now_v7(),
        evaluation: evaluation.id,
        level: MessageLevel::Error,
        message: failure.to_string(),
        source: Some(source.to_owned()),
        created_at: gradient_types::now(),
    }
    .into_active_model();

    EEvaluationMessage::insert(msg).exec(&ctx.worker_db).await?;

    update_evaluation_status(ctx, evaluation, status).await
}
