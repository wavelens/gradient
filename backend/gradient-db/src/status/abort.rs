/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{PhaseSubjectKind, record_phase_events};
use crate::{DbContext, fetch_in_chunks, for_each_chunk};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptOutcome, Column as CAttempt, Entity as EAttempt};
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, FromQueryResult, QueryFilter, QuerySelect};
use std::collections::HashSet;

#[tracing::instrument(level = "debug", skip_all)]
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
    let shared = wanted_by_other_evaluations(ctx, evaluation.id, &active_ids).await?;

    let to_abort: Vec<&MDerivationBuild> =
        active.iter().filter(|a| !shared.contains(&a.id)).collect();
    if to_abort.is_empty() {
        return Ok(Vec::new());
    }

    abort_shared_builds(ctx, evaluation, &to_abort).await
}

/// The abort only moves abortable shared builds. A failed attempt waiting for its retry stays as
/// it is, and its need has to go with the evaluation.
#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all, fields(shared_builds = to_abort.len()))]
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
        super::build_change(BuildStatus::ABORT)
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
        i16::from(BuildStatus::Aborted),
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

fn wanting_evaluations_sql() -> String {
    format!(
        "SELECT ob.derivation_build, ob.evaluation \
         FROM build_job ob {wanted} WHERE ob.derivation_build = ANY($1::uuid[])",
        wanted = crate::graph::can_start::wanted_by_its_evaluation("ob"),
    )
}

crate::sql_fn! {
    WANTING_EVALUATIONS = wanting_evaluations_sql,
        params = [SharedBuildIds(64)];
}

#[derive(Debug, FromQueryResult)]
pub(super) struct Wanting {
    pub derivation_build: uuid::Uuid,
    pub evaluation: uuid::Uuid,
}

#[tracing::instrument(level = "debug", skip_all)]
pub(super) async fn wanting_evaluations(
    ctx: &DbContext,
    shared_build_ids: &[DerivationBuildId],
) -> Result<Vec<Wanting>, sea_orm::DbErr> {
    fetch_in_chunks(shared_build_ids, |chunk| async move {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        Wanting::find_by_statement(WANTING_EVALUATIONS.bind([ids.into()]))
            .all(&ctx.worker_db)
            .await
    })
    .await
}

pub(super) async fn wanted_by_other_evaluations(
    ctx: &DbContext,
    this_eval: EvaluationId,
    shared_build_ids: &[DerivationBuildId],
) -> Result<HashSet<DerivationBuildId>, sea_orm::DbErr> {
    Ok(wanting_evaluations(ctx, shared_build_ids)
        .await?
        .into_iter()
        .filter(|w| w.evaluation != this_eval.into_inner())
        .map(|w| DerivationBuildId::from(w.derivation_build))
        .collect())
}

#[cfg(test)]
pub(super) fn wanting_row(
    evaluation: &MEvaluation,
    shared_build: DerivationBuildId,
) -> std::collections::BTreeMap<String, sea_orm::Value> {
    std::collections::BTreeMap::from([
        (
            "derivation_build".to_owned(),
            shared_build.into_inner().into(),
        ),
        ("evaluation".to_owned(), evaluation.id.into_inner().into()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkerDb;
    use crate::test_ctx::ctx;
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::{DatabaseConnection, MockExecResult, Value};
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

    fn scripted_db(
        shared_builds: Vec<BTreeMap<String, Value>>,
        active: Vec<MDerivationBuild>,
        wanting: Vec<BTreeMap<String, Value>>,
    ) -> DatabaseConnection {
        mock_db()
            .append_query_results([shared_builds])
            .append_query_results([active])
            .append_query_results([wanting])
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
    async fn an_abort_spares_a_shared_build_another_evaluation_wants() {
        let aborting = eval_row(EvaluationStatus::Aborted);
        let other = eval_row(EvaluationStatus::Failed);
        let (shared, shared_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let (mine, mine_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());

        let (ctx, pool) = ctx(scripted_db(
            vec![shared_build_id_row(shared), shared_build_id_row(mine)],
            vec![
                shared_build_row(shared, shared_drv),
                shared_build_row(mine, mine_drv),
            ],
            vec![wanting_row(&other, shared)],
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
    async fn the_aborting_evaluation_holds_none_of_its_own_builds_back() {
        let aborting = eval_row(EvaluationStatus::Building);
        let (shared, shared_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let (mine, mine_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());

        let (ctx, _pool) = ctx(scripted_db(
            vec![shared_build_id_row(shared), shared_build_id_row(mine)],
            vec![
                shared_build_row(shared, shared_drv),
                shared_build_row(mine, mine_drv),
            ],
            vec![wanting_row(&aborting, shared)],
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
            "only another evaluation's want spares a build"
        );
    }
}
