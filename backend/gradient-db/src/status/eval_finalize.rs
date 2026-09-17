/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Graph-derived evaluation finalization. An evaluation settles the moment its
//! last referenced anchor leaves the active set, regardless of WHICH mutation
//! path moved it - the effects emitter calls in here on every terminal
//! transition, so bulk sweeps and the single-row path finalize identically.

use super::evaluation_status::update_evaluation_status;
use crate::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashSet;
use tracing::info;

/// Settle `evaluation_id` if the build graph says it is done: no anchor it names
/// still blocks it ([`crate::graph_sql::blocks_evaluation`]). `Failed` when any
/// anchor terminally failed or the eval logged error-level messages (nix eval
/// errors mean a partially-successful walk), else `Completed`. A no-op unless the
/// evaluation is in its build phase.
pub async fn check_evaluation_done(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
) -> Result<(), DbErr> {
    let anchors = crate::reachability::eval_anchor_states(&ctx.worker_db, evaluation_id).await?;

    let any_active = anchors
        .iter()
        .any(|&(status, demanded)| crate::graph_sql::blocks_evaluation(status, demanded));
    if any_active {
        return Ok(());
    }

    let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
    else {
        return Ok(());
    };

    if !in_build_phase(&eval) {
        return Ok(());
    }

    let any_failed = anchors.iter().any(|(s, _)| {
        matches!(
            s,
            BuildStatus::FailedPermanent
                | BuildStatus::FailedTimeout
                | BuildStatus::DependencyFailed
        )
    });

    let eval_error_messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
        .filter(
            CEvaluationMessage::Level.eq(gradient_entity::evaluation_message::MessageLevel::Error),
        )
        .all(&ctx.worker_db)
        .await?;

    let target = if !any_failed && eval_error_messages.is_empty() {
        EvaluationStatus::Completed
    } else {
        EvaluationStatus::Failed
    };
    info!(
        %evaluation_id,
        ?target,
        any_failed,
        eval_errors = eval_error_messages.len(),
        "evaluation finished"
    );

    update_evaluation_status(ctx, eval, target).await;
    Ok(())
}

/// An evaluation whose builds are what it is waiting on: `Building`, or parked on
/// a reason the build phase owns. A pre-build park (`Approval`, `NoCache`, the
/// capacity and drain reasons) has named no anchors of its own yet, and an
/// `Aborting` park belongs to the abort, which writes the terminal status itself.
fn in_build_phase(eval: &MEvaluation) -> bool {
    match eval.status {
        EvaluationStatus::Building => true,
        EvaluationStatus::Waiting => matches!(
            eval.waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json),
            Some(WaitingReason::Workers { .. } | WaitingReason::GraphStuck { .. })
        ),
        _ => false,
    }
}

/// Finalize every evaluation referencing any of `derivations`, deduplicated.
pub async fn finalize_evals_for_derivations(
    ctx: &DbContext,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    let mut seen = HashSet::new();
    for &derivation in derivations {
        for evaluation_id in
            crate::reachability::evals_referencing_derivation(&ctx.worker_db, derivation).await?
        {
            if seen.insert(evaluation_id) {
                check_evaluation_done(ctx, evaluation_id).await?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn anchor(status: BuildStatus, demanded: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("status".to_owned(), Value::Int(Some(status as i32))),
            ("demanded".to_owned(), Value::Bool(Some(demanded))),
        ])
    }

    fn named() -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([(
            "derivation_build".to_owned(),
            Value::from(DerivationBuildId::now_v7().into_inner()),
        )])]
    }

    fn building() -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status: EvaluationStatus::Building,
            ..Default::default()
        }
    }

    /// The anchors an evaluation named but nothing demands are never built: no
    /// gate queues them and no event is coming, so an evaluation that waits for
    /// one waits forever (#666). They are settled work, and the evaluation that
    /// named them is done.
    #[tokio::test]
    async fn an_anchor_nothing_demands_does_not_hold_its_evaluation_open() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([named()])
            .append_query_results([vec![
                anchor(BuildStatus::Completed, true),
                anchor(BuildStatus::Created, false),
            ]])
            .append_query_results([vec![eval.clone()]])
            .append_query_results([Vec::<MEvaluationMessage>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![eval.clone()]])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        // The settle spawns the reactor hook and the phase event, each holding a
        // context clone: drop alone leaves the pool handle shared and the log
        // unreadable.
        crate::test_ctx::settle(ctx).await;

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log.iter().any(|s| s.contains(r#"UPDATE \"evaluation\""#)),
            "the evaluation settles on what is demanded of it: {log:?}"
        );
    }

    /// The same anchor with something still demanding it is work in flight, and
    /// the evaluation is not read at all until nothing blocks it.
    #[tokio::test]
    async fn a_demanded_anchor_holds_its_evaluation_open() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([named()])
            .append_query_results([vec![anchor(BuildStatus::Created, true)]])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        drop(ctx);

        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            2,
            "it reads the anchors it named and stops there: {log:?}"
        );
    }
}
