/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::abort::{abort_shared_builds, ids_shared_with_other_evaluations};
use super::emit_transition_effects;
use super::eval_finalize::check_evaluation_done;
use super::evaluation_status::reopen_evaluation;
use crate::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BuildRefusal {
    #[error("The evaluation of this build has already finished")]
    EvaluationFinished,
    #[error("The build is {0:?}")]
    WrongStatus(BuildStatus),
    #[error("Another running evaluation still needs this build")]
    NeededByOtherEvaluation,
    #[error("The worker has not confirmed the abort yet, try again shortly")]
    WorkerStillStopping,
    #[error("A newer evaluation of the task replaced this evaluation")]
    NewerEvaluation,
    #[error("No failed dependency of this build is retryable")]
    NoFailedDependency,
}

pub async fn abort_evaluation_build(
    ctx: &DbContext,
    evaluation: &MEvaluation,
    shared_build: &MDerivationBuild,
) -> Result<Result<(), BuildRefusal>, DbErr> {
    let open = !evaluation.status.is_terminal();
    if let Err(refusal) = check(open, shared_build, &BuildStatus::ABORTABLE) {
        return Ok(Err(refusal));
    }

    if !ids_shared_with_other_evaluations(ctx, evaluation.id, &[shared_build.id])
        .await?
        .is_empty()
    {
        return Ok(Err(BuildRefusal::NeededByOtherEvaluation));
    }

    mark_aborted(ctx, evaluation.id, &[shared_build.derivation], true).await?;
    abort_shared_builds(ctx, evaluation, &[shared_build]).await?;
    let failed =
        crate::graph::promotion::repair_dependency_failed(&ctx.worker_db, evaluation.id).await?;
    emit_transition_effects(ctx, &failed).await?;

    Ok(Ok(()))
}

pub async fn retry_evaluation_build(
    ctx: &DbContext,
    evaluation: &MEvaluation,
    shared_build: &MDerivationBuild,
) -> Result<Result<(), BuildRefusal>, DbErr> {
    let reopens = evaluation.status.is_terminal();
    let open = !reopens || reopenable(evaluation);
    if let Err(refusal) = check(open, shared_build, &BuildStatus::REQUEUEABLE) {
        return Ok(Err(refusal));
    }

    if reopens && !reopen_evaluation(ctx, evaluation).await? {
        return Ok(Err(BuildRefusal::NewerEvaluation));
    }

    let db = &ctx.worker_db;
    let thawed =
        crate::graph::promotion::retry_build_closure(db, evaluation.id, shared_build.derivation)
            .await?;
    let retried: Vec<DerivationId> = thawed
        .iter()
        .filter(|change| BuildStatus::RETRYABLE.contains(&change.from))
        .map(|change| change.derivation)
        .collect();
    mark_aborted(ctx, evaluation.id, &retried, false).await?;
    emit_transition_effects(ctx, &thawed).await?;
    let need = crate::graph::can_start::update_and_settle_need(db, &retried).await?;
    emit_transition_effects(ctx, &need.changes).await?;

    let failed = crate::graph::promotion::repair_dependency_failed(db, evaluation.id).await?;
    emit_transition_effects(ctx, &failed).await?;

    let thawed: Vec<DerivationId> = thawed.iter().map(|change| change.derivation).collect();
    let settled = crate::graph::can_start::settle_created(db, &thawed).await?;
    emit_transition_effects(ctx, &settled).await?;

    if retried.is_empty() {
        if reopens {
            check_evaluation_done(ctx, evaluation.id).await?;
        }
        return Ok(Err(BuildRefusal::NoFailedDependency));
    }

    Ok(Ok(()))
}

fn reopenable(evaluation: &MEvaluation) -> bool {
    EvaluationStatus::REOPENABLE.contains(&evaluation.status)
        && evaluation.building_started_at.is_some()
}

fn check(
    evaluation_open: bool,
    shared_build: &MDerivationBuild,
    allowed: &[BuildStatus],
) -> Result<(), BuildRefusal> {
    if !evaluation_open {
        return Err(BuildRefusal::EvaluationFinished);
    }

    if !allowed.contains(&shared_build.status) {
        return Err(BuildRefusal::WrongStatus(shared_build.status));
    }

    Ok(())
}

pub async fn wanted_again(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    mark_aborted(ctx, evaluation, derivations, false).await
}

async fn mark_aborted(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
    aborted: bool,
) -> Result<(), DbErr> {
    if derivations.is_empty() {
        return Ok(());
    }

    EBuildJob::update_many()
        .col_expr(CBuildJob::Aborted, Expr::value(aborted))
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.is_in(derivations.iter().copied()))
        .exec(&ctx.worker_db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_fixtures::mock::{mock_db, raw_statements, statements};
    use sea_orm::MockExecResult;

    fn evaluation(status: EvaluationStatus) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status,
            ..Default::default()
        }
    }

    fn shared_build(status: BuildStatus) -> MDerivationBuild {
        MDerivationBuild {
            id: DerivationBuildId::now_v7(),
            derivation: DerivationId::now_v7(),
            status,
            ..Default::default()
        }
    }

    #[test]
    fn only_a_failed_or_aborted_evaluation_past_its_evaluation_reopens() {
        let built = |status| MEvaluation {
            building_started_at: Some(gradient_types::now()),
            ..evaluation(status)
        };
        assert!(reopenable(&built(EvaluationStatus::Failed)));
        assert!(reopenable(&built(EvaluationStatus::Aborted)));
        assert!(!reopenable(&built(EvaluationStatus::Completed)));
        assert!(!reopenable(&evaluation(EvaluationStatus::Aborted)));
    }

    #[test]
    fn a_closed_evaluation_refuses_before_the_build_status_counts() {
        assert_eq!(
            check(
                false,
                &shared_build(BuildStatus::Completed),
                &BuildStatus::REQUEUEABLE
            ),
            Err(BuildRefusal::EvaluationFinished)
        );
        assert_eq!(
            check(
                true,
                &shared_build(BuildStatus::Completed),
                &BuildStatus::REQUEUEABLE
            ),
            Err(BuildRefusal::WrongStatus(BuildStatus::Completed))
        );
    }

    #[tokio::test]
    async fn a_newer_evaluation_of_the_task_keeps_a_failed_evaluation_closed() {
        let failed = MEvaluation {
            building_started_at: Some(gradient_types::now()),
            ..evaluation(EvaluationStatus::Failed)
        };
        let build = shared_build(BuildStatus::DependencyFailed);
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let outcome = retry_evaluation_build(&ctx, &failed, &build)
            .await
            .expect("the reopen runs");
        drop(ctx);

        assert_eq!(outcome, Err(BuildRefusal::NewerEvaluation));
        let log = raw_statements(pool.into_transaction_log());
        assert_eq!(log.len(), 1, "nothing but the reopen runs: {log:?}");
        assert!(log[0].sql.starts_with("UPDATE evaluation e"), "{log:?}");
    }

    #[tokio::test]
    async fn an_abort_leaves_a_build_another_running_evaluation_needs() {
        let aborting = evaluation(EvaluationStatus::Building);
        let other = evaluation(EvaluationStatus::Waiting);
        let build = shared_build(BuildStatus::Building);
        let db = mock_db()
            .append_query_results([vec![MBuildJob {
                id: BuildJobId::now_v7(),
                evaluation: other.id,
                derivation: build.derivation,
                derivation_build: build.id,
                ..Default::default()
            }]])
            .append_query_results([vec![other]])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let outcome = abort_evaluation_build(&ctx, &aborting, &build)
            .await
            .expect("the check runs");
        drop(ctx);

        assert_eq!(outcome, Err(BuildRefusal::NeededByOtherEvaluation));
        let log = statements(pool.into_transaction_log());
        assert!(
            !log.iter().any(|s| s.starts_with("UPDATE")),
            "nothing is written: {log:?}"
        );
    }
}
