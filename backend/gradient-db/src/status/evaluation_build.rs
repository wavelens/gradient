/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::abort::{abort_shared_builds, ids_shared_with_other_evaluations};
use super::emit_transition_effects;
use crate::DbContext;
use crate::state_machine::EvalStateMachine;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildRefusal {
    EvaluationFinished,
    WrongStatus(BuildStatus),
    NeededByOtherEvaluation,
    WorkerStillStopping,
}

pub async fn abort_evaluation_build(
    ctx: &DbContext,
    evaluation: &MEvaluation,
    shared_build: &MDerivationBuild,
) -> Result<Result<(), BuildRefusal>, DbErr> {
    if let Err(refusal) = check(evaluation, shared_build, &BuildStatus::ABORTABLE) {
        return Ok(Err(refusal));
    }

    if !ids_shared_with_other_evaluations(ctx, evaluation.id, &[shared_build.id])
        .await?
        .is_empty()
    {
        return Ok(Err(BuildRefusal::NeededByOtherEvaluation));
    }

    mark_aborted(ctx, evaluation.id, shared_build.derivation, true).await?;
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
    if let Err(refusal) = check(evaluation, shared_build, &BuildStatus::RETRYABLE) {
        return Ok(Err(refusal));
    }

    let db = &ctx.worker_db;
    mark_aborted(ctx, evaluation.id, shared_build.derivation, false).await?;
    let thawed =
        crate::graph::promotion::retry_build_closure(db, evaluation.id, shared_build.derivation)
            .await?;
    emit_transition_effects(ctx, &thawed).await?;

    let failed = crate::graph::promotion::repair_dependency_failed(db, evaluation.id).await?;
    emit_transition_effects(ctx, &failed).await?;

    let queued = crate::graph::can_start::promote_closure(db, evaluation.id).await?;
    emit_transition_effects(ctx, &queued).await?;

    Ok(Ok(()))
}

fn check(
    evaluation: &MEvaluation,
    shared_build: &MDerivationBuild,
    allowed: &[BuildStatus],
) -> Result<(), BuildRefusal> {
    if EvalStateMachine::is_terminal(&evaluation.status) {
        return Err(BuildRefusal::EvaluationFinished);
    }

    if !allowed.contains(&shared_build.status) {
        return Err(BuildRefusal::WrongStatus(shared_build.status));
    }

    Ok(())
}

async fn mark_aborted(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivation: DerivationId,
    aborted: bool,
) -> Result<(), DbErr> {
    EBuildJob::update_many()
        .col_expr(CBuildJob::Aborted, Expr::value(aborted))
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.eq(derivation))
        .exec(&ctx.worker_db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_entity::evaluation::EvaluationStatus;
    use sea_orm::{DatabaseBackend, MockDatabase};

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
    fn a_finished_evaluation_takes_no_abort_and_no_retry() {
        let finished = evaluation(EvaluationStatus::Failed);
        for (status, allowed) in [
            (BuildStatus::Building, &BuildStatus::ABORTABLE[..]),
            (BuildStatus::FailedPermanent, &BuildStatus::RETRYABLE[..]),
        ] {
            assert_eq!(
                check(&finished, &shared_build(status), allowed),
                Err(BuildRefusal::EvaluationFinished)
            );
        }
    }

    #[test]
    fn only_a_running_build_is_aborted_and_only_a_stopped_build_retried() {
        let running = evaluation(EvaluationStatus::Building);
        assert_eq!(
            check(
                &running,
                &shared_build(BuildStatus::Completed),
                &BuildStatus::ABORTABLE
            ),
            Err(BuildRefusal::WrongStatus(BuildStatus::Completed))
        );
        assert_eq!(
            check(
                &running,
                &shared_build(BuildStatus::DependencyFailed),
                &BuildStatus::RETRYABLE
            ),
            Err(BuildRefusal::WrongStatus(BuildStatus::DependencyFailed))
        );
        assert_eq!(
            check(
                &running,
                &shared_build(BuildStatus::Aborted),
                &BuildStatus::RETRYABLE
            ),
            Ok(())
        );
    }

    #[tokio::test]
    async fn an_abort_leaves_a_build_another_running_evaluation_needs() {
        let aborting = evaluation(EvaluationStatus::Building);
        let other = evaluation(EvaluationStatus::Waiting);
        let build = shared_build(BuildStatus::Building);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            !log.iter().any(|s| s.starts_with("UPDATE")),
            "nothing is written: {log:?}"
        );
    }
}
