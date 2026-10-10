/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{PhaseSubjectKind, record_phase_events};
use crate::state_machine::EvalStateMachine;
use crate::{DbContext, fetch_in_chunks, for_each_chunk};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptOutcome, Column as CAttempt, Entity as EAttempt};
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;

pub async fn abort_eval_shared_builds(
    ctx: &DbContext,
    evaluation: &MEvaluation,
) -> Result<Vec<DerivationBuildId>, sea_orm::DbErr> {
    let shared_build_ids: Vec<DerivationBuildId> = EBuildJob::find()
        .select_only()
        .column(CBuildJob::DerivationBuild)
        .filter(CBuildJob::Evaluation.eq(evaluation.id))
        .into_tuple::<DerivationBuildId>()
        .all(&ctx.worker_db)
        .await?;
    if shared_build_ids.is_empty() {
        return Ok(Vec::new());
    }

    let active = fetch_in_chunks(&shared_build_ids, |chunk| async move {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Id.is_in(chunk))
            .filter(CDerivationBuild::Status.is_in(BuildStatus::ABORTABLE))
            .all(&ctx.worker_db)
            .await
    })
    .await?;
    if active.is_empty() {
        return Ok(Vec::new());
    }

    let active_ids: Vec<DerivationBuildId> = active.iter().map(|a| a.id).collect();
    let shared = ids_shared_with_other_evaluations(ctx, evaluation.id, &active_ids).await?;

    let to_abort: Vec<&MDerivationBuild> =
        active.iter().filter(|a| !shared.contains(&a.id)).collect();
    if to_abort.is_empty() {
        return Ok(Vec::new());
    }

    abort_shared_builds(ctx, evaluation, &to_abort).await
}

/// The abort only moves abortable shared builds. A failed attempt waiting for its retry stays as
/// it is, and its need has to go with the evaluation.
pub async fn release_evaluation_need(
    ctx: &DbContext,
    evaluation: EvaluationId,
) -> Result<(), sea_orm::DbErr> {
    let entry_points: Vec<DerivationId> = EEntryPoint::find()
        .select_only()
        .column(CEntryPoint::Derivation)
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .into_tuple::<DerivationId>()
        .all(&ctx.worker_db)
        .await?;
    for chunk in entry_points.chunks(crate::IN_CHUNK_SIZE) {
        let settled =
            crate::graph::can_start::update_and_settle_need(&ctx.worker_db, chunk).await?;
        super::emit_transition_effects(ctx, &settled.changes).await?;
    }

    Ok(())
}

pub(super) async fn abort_shared_builds(
    ctx: &DbContext,
    evaluation: &MEvaluation,
    to_abort: &[&MDerivationBuild],
) -> Result<Vec<DerivationBuildId>, sea_orm::DbErr> {
    let abort_ids: Vec<DerivationBuildId> = to_abort.iter().map(|a| a.id).collect();
    let building_ids: Vec<DerivationBuildId> = to_abort
        .iter()
        .filter(|a| a.status == BuildStatus::Building)
        .map(|a| a.id)
        .collect();
    let now = gradient_types::now();

    for_each_chunk(&abort_ids, |chunk| async move {
        EDerivationBuild::update_many()
            .col_expr(CDerivationBuild::Status, Expr::value(BuildStatus::Aborted))
            .col_expr(CDerivationBuild::UpdatedAt, Expr::value(now))
            .filter(CDerivationBuild::Id.is_in(chunk))
            .exec(&ctx.worker_db)
            .await
    })
    .await?;

    // This bulk transition is bypassing `update_derivation_build_status`.
    // Its exact changes must go through the one emitter.
    let changes: Vec<super::TransitionChange> = to_abort
        .iter()
        .map(|a| super::TransitionChange {
            derivation: a.derivation,
            from: a.status,
            to: BuildStatus::Aborted,
        })
        .collect();
    super::emit_transition_effects(ctx, &changes).await?;

    if !building_ids.is_empty() {
        for_each_chunk(&building_ids, |chunk| async move {
            EAttempt::update_many()
                .col_expr(CAttempt::Outcome, Expr::value(AttemptOutcome::Aborted))
                .col_expr(CAttempt::BuildFinishedAt, Expr::value(Some(now)))
                .filter(CAttempt::DerivationBuild.is_in(chunk))
                .filter(CAttempt::BuildFinishedAt.is_null())
                .exec(&ctx.worker_db)
                .await
        })
        .await?;
    }

    let pe_ids: Vec<uuid::Uuid> = abort_ids.iter().map(|id| id.into_inner()).collect();
    record_phase_events(
        &ctx.worker_db,
        PhaseSubjectKind::Build,
        &pe_ids,
        i32::from(BuildStatus::Aborted) as i16,
        now,
    )
    .await?;

    ctx.events
        .publish(gradient_types::events::evaluation::Progress {
            evaluation_id: evaluation.id,
            task: evaluation.task,
        });

    Ok(abort_ids)
}

pub(super) async fn ids_shared_with_other_evaluations(
    ctx: &DbContext,
    this_eval: EvaluationId,
    shared_build_ids: &[DerivationBuildId],
) -> Result<HashSet<DerivationBuildId>, sea_orm::DbErr> {
    let other_jobs = fetch_in_chunks(shared_build_ids, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::DerivationBuild.is_in(chunk))
            .filter(CBuildJob::Evaluation.ne(this_eval))
            .all(&ctx.worker_db)
            .await
    })
    .await?;
    if other_jobs.is_empty() {
        return Ok(HashSet::new());
    }

    let other_eval_ids: Vec<EvaluationId> = other_jobs
        .iter()
        .map(|j| j.evaluation)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let evals = fetch_in_chunks(&other_eval_ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Id.is_in(chunk))
            .all(&ctx.worker_db)
            .await
    })
    .await?;
    let live: HashSet<EvaluationId> = evals
        .into_iter()
        .filter(|e| !EvalStateMachine::is_terminal(&e.status))
        .map(|e| e.id)
        .collect();

    Ok(other_jobs
        .into_iter()
        .filter(|j| live.contains(&j.evaluation))
        .map(|j| j.derivation_build)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkerDb;
    use crate::test_ctx::ctx;
    use gradient_entity::evaluation::EvaluationStatus;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn eval_row(status: EvaluationStatus) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status,
            ..Default::default()
        }
    }

    fn shared_build_row(id: DerivationBuildId, derivation: DerivationId) -> MDerivationBuild {
        MDerivationBuild {
            id,
            derivation,
            status: BuildStatus::Building,
            ..Default::default()
        }
    }

    fn shared_build_id_row(id: DerivationBuildId) -> BTreeMap<String, Value> {
        BTreeMap::from([("derivation_build".to_owned(), Value::from(id.into_inner()))])
    }

    fn job_row(
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
        derivation: DerivationId,
    ) -> MBuildJob {
        MBuildJob {
            id: BuildJobId::now_v7(),
            evaluation,
            derivation,
            derivation_build: shared_build,
            ..Default::default()
        }
    }

    fn scripted_db(
        shared_builds: Vec<BTreeMap<String, Value>>,
        active: Vec<MDerivationBuild>,
        other_jobs: Vec<MBuildJob>,
        other_evals: Vec<MEvaluation>,
    ) -> DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([shared_builds])
            .append_query_results([active])
            .append_query_results([other_jobs])
            .append_query_results([other_evals])
            .append_query_results(vec![Vec::<MBuildJob>::new(); 4])
            .append_query_results([crate::test_ctx::inserted_phase_event()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                12
            ])
            .into_connection()
    }

    fn abort_update(pool: WorkerDb) -> String {
        pool.into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .find(|s| s.contains(r#"UPDATE \"derivation_build\""#))
            .expect("the abort writes derivation_build")
    }

    #[tokio::test]
    async fn an_abort_spares_a_shared_build_another_live_evaluation_needs() {
        let aborting = eval_row(EvaluationStatus::Building);
        let other = eval_row(EvaluationStatus::Building);
        let (shared, shared_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let (mine, mine_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());

        let (ctx, pool) = ctx(scripted_db(
            vec![shared_build_id_row(shared), shared_build_id_row(mine)],
            vec![
                shared_build_row(shared, shared_drv),
                shared_build_row(mine, mine_drv),
            ],
            vec![job_row(other.id, shared, shared_drv)],
            vec![other],
        ))
        .await;

        let aborted = abort_eval_shared_builds(&ctx, &aborting)
            .await
            .expect("the abort runs");

        assert_eq!(
            aborted,
            vec![mine],
            "only the shared build no other evaluation wants is aborted"
        );

        drop(ctx);
        let update = abort_update(pool);
        assert!(
            update.contains(&mine.to_string()),
            "the exclusive shared build is aborted: {update}"
        );
        assert!(
            !update.contains(&shared.to_string()),
            "the build both evaluations share must keep building for the other evaluation: {update}"
        );
    }

    #[tokio::test]
    async fn an_abort_stops_a_shared_build_once_the_other_evaluation_is_terminal() {
        let aborting = eval_row(EvaluationStatus::Building);
        let other = eval_row(EvaluationStatus::Completed);
        let (shared, shared_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let (mine, mine_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());

        let (ctx, _pool) = ctx(scripted_db(
            vec![shared_build_id_row(shared), shared_build_id_row(mine)],
            vec![
                shared_build_row(shared, shared_drv),
                shared_build_row(mine, mine_drv),
            ],
            vec![job_row(other.id, shared, shared_drv)],
            vec![other],
        ))
        .await;

        let mut aborted = abort_eval_shared_builds(&ctx, &aborting)
            .await
            .expect("the abort runs");
        aborted.sort();

        let mut expected = vec![shared, mine];
        expected.sort();
        assert_eq!(
            aborted, expected,
            "a terminal evaluation holds nothing back"
        );
    }
}
