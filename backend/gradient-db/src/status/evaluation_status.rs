/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_message::EvaluationFailure;
use super::logging::{PhaseSubjectKind, insert_evaluation_message, record_phase_event};
use crate::DbContext;
use crate::sql::StatusWrite;
use chrono::NaiveDateTime;
use gradient_entity::evaluation::{AbortReason, EvaluationStatus};
use gradient_entity::transition::Transition;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, UpdateMany};
use tracing::{debug, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationAbortReasonWithUserIdAndReplacementId {
    pub reason: AbortReason,
    pub aborted_by: Option<UserId>,
    pub superseded_by: Option<EvaluationId>,
}

impl EvaluationAbortReasonWithUserIdAndReplacementId {
    pub fn by_user(aborted_by: Option<UserId>) -> Self {
        Self {
            reason: AbortReason::User,
            aborted_by,
            superseded_by: None,
        }
    }

    pub fn superseded(superseded_by: Option<EvaluationId>) -> Self {
        Self {
            reason: AbortReason::Superseded,
            aborted_by: None,
            superseded_by,
        }
    }

    pub fn because(reason: AbortReason) -> Self {
        Self {
            reason,
            aborted_by: None,
            superseded_by: None,
        }
    }

    pub fn columns(self, update: UpdateMany<EEvaluation>) -> UpdateMany<EEvaluation> {
        update
            .col_expr(CEvaluation::AbortReason, Expr::value(self.reason))
            .col_expr(CEvaluation::AbortedBy, Expr::value(self.aborted_by))
            .col_expr(CEvaluation::SupersededBy, Expr::value(self.superseded_by))
    }
}

pub async fn stamp_superseded_by_on_aborted_evaluations<C: ConnectionTrait>(
    db: &C,
    aborted: &[EvaluationId],
    newer: EvaluationId,
) -> Result<(), DbErr> {
    if aborted.is_empty() {
        return Ok(());
    }

    EEvaluation::update_many()
        .col_expr(CEvaluation::SupersededBy, Expr::value(newer))
        .filter(CEvaluation::Id.is_in(aborted.iter().copied()))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Aborted))
        .filter(CEvaluation::AbortReason.eq(AbortReason::Superseded))
        .filter(CEvaluation::SupersededBy.is_null())
        .exec(db)
        .await?;
    Ok(())
}

pub async fn update_evaluation_status(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> Result<MEvaluation, DbErr> {
    update_status_with(ctx, evaluation, status, |update| update).await
}

pub async fn mark_evaluation_aborted(
    ctx: &DbContext,
    evaluation: MEvaluation,
    abort: EvaluationAbortReasonWithUserIdAndReplacementId,
) -> Result<MEvaluation, DbErr> {
    update_status_with(ctx, evaluation, EvaluationStatus::Aborted, |update| {
        abort.columns(update)
    })
    .await
}

async fn update_status_with(
    ctx: &DbContext,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    extra_columns: impl FnOnce(UpdateMany<EEvaluation>) -> UpdateMany<EEvaluation>,
) -> Result<MEvaluation, DbErr> {
    // All build transitions embed this future, so we box the future. The size of the unboxed future can overflow the stack in debug builds.
    match Box::pin(update_evaluation_status_only_if_changed(
        ctx,
        &evaluation,
        status,
        extra_columns,
    ))
    .await?
    {
        Some(updated) => Ok(updated),
        None => read_evaluation_row_or_return_given_evaluation(ctx, evaluation).await,
    }
}

async fn read_evaluation_row_or_return_given_evaluation(
    ctx: &DbContext,
    evaluation: MEvaluation,
) -> Result<MEvaluation, DbErr> {
    Ok(EEvaluation::find_by_id(evaluation.id)
        .one(&ctx.worker_db)
        .await?
        .unwrap_or(evaluation))
}

async fn update_evaluation_status_only_if_changed(
    ctx: &DbContext,
    evaluation: &MEvaluation,
    status: EvaluationStatus,
    extra_columns: impl FnOnce(UpdateMany<EEvaluation>) -> UpdateMany<EEvaluation>,
) -> Result<Option<MEvaluation>, DbErr> {
    if evaluation.status != status && !status.sources().contains(&evaluation.status) {
        warn!(evaluation_id = %evaluation.id, from = ?evaluation.status, to = ?status, "Skipping invalid evaluation status transition");
        return Ok(None);
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
        update = update.col_expr(
            col,
            sea_orm::sea_query::Expr::case(
                CEvaluation::Status.eq(status),
                sea_orm::sea_query::Expr::col(col),
            )
            .finally(now)
            .into(),
        );
    }

    let updated = extra_columns(update)
        .filter(CEvaluation::Id.eq(evaluation.id))
        .filter(CEvaluation::Status.is_in(sources_in_the_row(status)))
        .exec(&ctx.worker_db)
        .await?;

    if updated.rows_affected == 0 {
        return Ok(None);
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
    Ok(Some(updated_eval))
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

fn reopen_evaluation_for_exec_job_sql() -> String {
    let reopen = EvaluationStatus::REOPEN_FOR_EXEC_JOB;
    format!(
        "UPDATE evaluation e \
         SET status = {building}, finished_at = NULL, waiting_reason = NULL, \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE e.id = $1 AND e.status IN ({reopenable}) \
           AND NOT EXISTS (\
             SELECT 1 FROM evaluation o \
             WHERE o.task = e.task AND o.id <> e.id \
               AND (o.created_at > e.created_at OR o.status NOT IN ({terminal})))",
        building = crate::sql::status::to(reopen),
        reopenable = crate::sql::status::from(reopen),
        terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

crate::sql_fn! {
    REOPEN_EVALUATION_FOR_EXEC_JOB = reopen_evaluation_for_exec_job_sql,
        params = [EvaluationId],
        status_write = StatusWrite::evaluation(EvaluationStatus::REOPEN_FOR_EXEC_JOB);
}

pub async fn reopen_evaluation_for_exec_job(
    ctx: &DbContext,
    evaluation: &MEvaluation,
) -> Result<bool, DbErr> {
    let reopened = ctx
        .worker_db
        .execute_raw(REOPEN_EVALUATION_FOR_EXEC_JOB.bind([evaluation.id.into_inner().into()]))
        .await?;
    if reopened.rows_affected() == 0 {
        return Ok(false);
    }

    crate::evaluations::counters::recount_evaluations(&ctx.worker_db, &[evaluation.id]).await?;
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
    ctx.wake.delivery.notify_one();

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

    let Some(updated) = Box::pin(update_evaluation_status_only_if_changed(
        ctx,
        &evaluation,
        status,
        |update| update,
    ))
    .await?
    else {
        return read_evaluation_row_or_return_given_evaluation(ctx, evaluation).await;
    };

    insert_evaluation_message(
        &ctx.worker_db,
        evaluation.id,
        MessageLevel::Error,
        failure.to_string(),
        Some(source.to_owned()),
    )
    .await?;

    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::shape::shape;

    #[tokio::test]
    async fn outdated_error_report_after_completion_keeps_completed_status_without_error_message() {
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .evaluation_status(EvaluationStatus::Completed)
            .into_database()
            .await;
        let outdated = MEvaluation {
            status: EvaluationStatus::Building,
            ..db.find::<EEvaluation>(evaluation)
                .await
                .expect("the evaluation exists")
        };
        let (ctx, _pool) = crate::test_ctx::ctx(db.connection()).await;

        let returned = update_evaluation_status_with_error(
            &ctx,
            outdated,
            EvaluationStatus::Failed,
            &EvaluationFailure::ReportedByWorker("late"),
        )
        .await
        .expect("the update runs");

        assert_eq!(returned.status, EvaluationStatus::Completed);
        assert!(db.error_messages(evaluation).await.is_empty());
        drop(ctx);
        db.finish().await;
    }

    #[tokio::test]
    async fn reporting_same_evaluation_status_keeps_building_started_at() {
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .evaluation_status(EvaluationStatus::Building)
            .into_database()
            .await;
        let started = chrono::DateTime::UNIX_EPOCH.naive_utc();
        db.execute(&format!(
            "UPDATE evaluation SET building_started_at = '{started}' WHERE id = '{evaluation}'"
        ))
        .await;
        let current = db
            .find::<EEvaluation>(evaluation)
            .await
            .expect("the evaluation exists");
        let (ctx, _pool) = crate::test_ctx::ctx(db.connection()).await;

        update_evaluation_status(&ctx, current, EvaluationStatus::Building)
            .await
            .expect("the update runs");

        let row = db
            .find::<EEvaluation>(evaluation)
            .await
            .expect("the evaluation exists");
        assert_eq!(row.building_started_at, Some(started));
        drop(ctx);
        db.finish().await;
    }
}
