/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_status::update_evaluation_status;
use super::logging::{PhaseSubjectKind, finalize_build_log, record_phase_events};
use crate::dep_closure::reconcile_eval_dep_counts;
use crate::state_machine::EvalStateMachine;
use crate::{DbContext, fetch_in_chunks, for_each_chunk};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptOutcome, Column as CAttempt, Entity as EAttempt};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;
use tracing::error;

/// Abort an evaluation's in-flight builds. Anchors are global, so this only
/// aborts the anchors this evaluation needs that no other live evaluation also
/// needs; anchors still wanted elsewhere keep running for those evaluations.
pub async fn abort_evaluation(ctx: &DbContext, evaluation: MEvaluation) -> Vec<DerivationBuildId> {
    if EvalStateMachine::is_terminal(&evaluation.status) {
        return Vec::new();
    }

    // Park the evaluation first: the dispatcher skips Waiting evaluations, so
    // this stops new work being handed out while we abort. The terminal
    // transition to Aborted below carries the user-facing side effects.
    gate_evaluation_aborting(ctx, evaluation.id).await;

    let aborted = match abort_eval_anchors(ctx, &evaluation).await {
        Ok(aborted) => aborted,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to abort evaluation anchors");
            Vec::new()
        }
    };

    update_evaluation_status(ctx, evaluation, EvaluationStatus::Aborted).await;
    aborted
}

/// Park the evaluation as `Waiting` with the `Aborting` reason via a direct
/// filtered update; the eventual transition to `Aborted` carries the side effects.
async fn gate_evaluation_aborting(ctx: &DbContext, evaluation_id: EvaluationId) {
    let res = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Waiting))
        .col_expr(
            CEvaluation::WaitingReason,
            Expr::value(WaitingReason::Aborting.to_json()),
        )
        .col_expr(CEvaluation::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CEvaluation::Id.eq(evaluation_id))
        .filter(CEvaluation::Status.is_not_in([
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ]))
        .exec(&ctx.worker_db)
        .await;

    if let Err(e) = res {
        error!(error = %e, %evaluation_id, "Failed to park evaluation for abort");
    }
}

/// Abort every anchor only `evaluation` still needs, returning the ids it moved.
pub async fn abort_eval_anchors(
    ctx: &DbContext,
    evaluation: &MEvaluation,
) -> Result<Vec<DerivationBuildId>, sea_orm::DbErr> {
    let anchor_ids: Vec<DerivationBuildId> = EBuildJob::find()
        .select_only()
        .column(CBuildJob::DerivationBuild)
        .filter(CBuildJob::Evaluation.eq(evaluation.id))
        .into_tuple::<DerivationBuildId>()
        .all(&ctx.worker_db)
        .await?;
    if anchor_ids.is_empty() {
        return Ok(Vec::new());
    }

    let active = fetch_in_chunks(&anchor_ids, |chunk| async move {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Id.is_in(chunk))
            .filter(CDerivationBuild::Status.is_in([
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .all(&ctx.worker_db)
            .await
    })
    .await?;
    if active.is_empty() {
        return Ok(Vec::new());
    }

    let active_ids: Vec<DerivationBuildId> = active.iter().map(|a| a.id).collect();
    let shared = shared_anchor_ids(ctx, evaluation.id, &active_ids).await?;

    let to_abort: Vec<&MDerivationBuild> =
        active.iter().filter(|a| !shared.contains(&a.id)).collect();
    if to_abort.is_empty() {
        return Ok(Vec::new());
    }

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

    // This bulk transition bypasses `update_derivation_build_status`; feed the
    // exact changes (pre-status was selected above) through the one emitter.
    let changes: Vec<super::TransitionChange> = to_abort
        .iter()
        .map(|a| super::TransitionChange {
            derivation: a.derivation,
            from: a.status,
            to: BuildStatus::Aborted,
        })
        .collect();
    super::emit_transition_effects(ctx, &changes).await;

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

    reconcile_eval_dep_counts(&ctx.worker_db, evaluation.id).await?;

    let pe_ids: Vec<uuid::Uuid> = abort_ids.iter().map(|id| id.into_inner()).collect();
    record_phase_events(
        &ctx.worker_db,
        PhaseSubjectKind::Build,
        &pe_ids,
        i32::from(BuildStatus::Aborted) as i16,
        now,
    )
    .await;

    finalize_aborted_logs(ctx, &building_ids).await;

    let _ = ctx
        .board_events
        .send(gradient_types::BoardEvent::EvaluationProgress {
            task: evaluation.task.map(|p| p.into_inner()),
            evaluation_id: evaluation.id.into_inner(),
        });

    Ok(abort_ids)
}

/// Of `anchor_ids`, those a non-terminal evaluation other than `this_eval` still
/// needs (via its own `build_job`). Those anchors must keep running.
async fn shared_anchor_ids(
    ctx: &DbContext,
    this_eval: EvaluationId,
    anchor_ids: &[DerivationBuildId],
) -> Result<HashSet<DerivationBuildId>, sea_orm::DbErr> {
    let other_jobs = fetch_in_chunks(anchor_ids, |chunk| async move {
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

/// Compress the log of each executing anchor that was aborted. Spawned so log
/// I/O never blocks the abort; Created/Queued anchors never produced a log.
async fn finalize_aborted_logs(ctx: &DbContext, building_ids: &[DerivationBuildId]) {
    if building_ids.is_empty() {
        return;
    }

    let attempts = crate::build_attempt::latest_attempts(&ctx.worker_db, building_ids)
        .await
        .unwrap_or_default();
    for &anchor_id in building_ids {
        if let Some(att) = attempts.get(&anchor_id) {
            let attempt_id = att.id;
            let log_ctx = ctx.detached();
            ctx.shutdown.spawn(async move {
                finalize_build_log(&log_ctx, attempt_id).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkerDb;
    use crate::test_ctx::ctx;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn eval_row(status: EvaluationStatus) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status,
            ..Default::default()
        }
    }

    fn anchor_row(id: DerivationBuildId, derivation: DerivationId) -> MDerivationBuild {
        MDerivationBuild {
            id,
            derivation,
            status: BuildStatus::Building,
            ..Default::default()
        }
    }

    /// The opening select projects a single column, and a mock row is read by
    /// position, so only its shape matters here.
    fn anchor_id_row(id: DerivationBuildId) -> BTreeMap<String, Value> {
        BTreeMap::from([("derivation_build".to_owned(), Value::from(id.into_inner()))])
    }

    fn job_row(
        evaluation: EvaluationId,
        anchor: DerivationBuildId,
        derivation: DerivationId,
    ) -> MBuildJob {
        MBuildJob {
            id: BuildJobId::now_v7(),
            evaluation,
            derivation,
            derivation_build: anchor,
            ..Default::default()
        }
    }

    /// The query script `abort_eval_anchors` replays, in order: the aborting
    /// evaluation's anchors, which of them are still active, the `build_job`
    /// rows other evaluations hold on those anchors, and those evaluations.
    /// Everything past the abort write (dep-count deltas, board events, phase
    /// events, log finalize) is answered empty: the decision is made by then and
    /// each of those paths is a no-op on empty input.
    fn scripted_db(
        anchors: Vec<BTreeMap<String, Value>>,
        active: Vec<MDerivationBuild>,
        other_jobs: Vec<MBuildJob>,
        other_evals: Vec<MEvaluation>,
    ) -> DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([anchors])
            .append_query_results([active])
            .append_query_results([other_jobs])
            .append_query_results([other_evals])
            .append_query_results(vec![Vec::<MBuildJob>::new(); 6])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                12
            ])
            .into_connection()
    }

    /// The `UPDATE derivation_build` the abort wrote, rendered with its bound ids.
    fn abort_update(pool: WorkerDb) -> String {
        pool.into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .find(|s| s.contains(r#"UPDATE \"derivation_build\""#))
            .expect("the abort writes derivation_build")
    }

    /// Two live evaluations building the same derivation share its anchor, so
    /// aborting one may only stop the anchors it alone still wants. The shared
    /// one stays Building and is never written, which is what keeps the other
    /// evaluation's build running instead of restarting it later.
    #[tokio::test]
    async fn an_abort_spares_an_anchor_another_live_evaluation_needs() {
        let aborting = eval_row(EvaluationStatus::Building);
        let other = eval_row(EvaluationStatus::Building);
        let (shared, shared_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let (mine, mine_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());

        let (ctx, pool) = ctx(scripted_db(
            vec![anchor_id_row(shared), anchor_id_row(mine)],
            vec![anchor_row(shared, shared_drv), anchor_row(mine, mine_drv)],
            vec![job_row(other.id, shared, shared_drv)],
            vec![other],
        ))
        .await;

        let aborted = abort_eval_anchors(&ctx, &aborting)
            .await
            .expect("the abort runs");

        assert_eq!(
            aborted,
            vec![mine],
            "only the anchor no other evaluation wants is aborted"
        );

        drop(ctx);
        let update = abort_update(pool);
        assert!(
            update.contains(&mine.to_string()),
            "the exclusive anchor is aborted: {update}"
        );
        assert!(
            !update.contains(&shared.to_string()),
            "the shared anchor must keep building for the other evaluation: {update}"
        );
    }

    /// Same graph, but the other evaluation has already finished: nothing live
    /// needs the shared anchor any more, so the abort takes both.
    #[tokio::test]
    async fn an_abort_stops_a_shared_anchor_once_the_other_evaluation_is_terminal() {
        let aborting = eval_row(EvaluationStatus::Building);
        let other = eval_row(EvaluationStatus::Completed);
        let (shared, shared_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let (mine, mine_drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());

        let (ctx, _pool) = ctx(scripted_db(
            vec![anchor_id_row(shared), anchor_id_row(mine)],
            vec![anchor_row(shared, shared_drv), anchor_row(mine, mine_drv)],
            vec![job_row(other.id, shared, shared_drv)],
            vec![other],
        ))
        .await;

        let mut aborted = abort_eval_anchors(&ctx, &aborting)
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
