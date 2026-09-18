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
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};
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
    // The blocking question first, and as an EXISTS: it is asked on every terminal
    // transition and the answer is almost always "yes" off the first blocking
    // anchor, so neither the anchor set nor the evaluation row is worth reading
    // until it comes back false.
    if crate::reachability::eval_blocked(&ctx.worker_db, evaluation_id).await? {
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

    let any_failed =
        crate::reachability::eval_any_anchor_failed(&ctx.worker_db, evaluation_id).await?;

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
///
/// The referencing set is resolved for the whole batch in one read rather than one
/// per derivation: a demand loss names as many derivations as the recompute moved,
/// and the union is all this wants.
pub async fn finalize_evals_for_derivations(
    ctx: &DbContext,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    let evaluations =
        crate::reachability::evals_referencing_derivations(&ctx.worker_db, derivations).await?;
    for evaluation_id in evaluations {
        check_evaluation_done(ctx, evaluation_id).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    /// The reply to either `EXISTS` read: one row, one boolean column.
    fn flag(column: &str, value: bool) -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([(
            column.to_owned(),
            Value::Bool(Some(value)),
        )])]
    }

    fn building() -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status: EvaluationStatus::Building,
            ..Default::default()
        }
    }

    /// Nothing blocking left settles the evaluation, and the failure question is
    /// asked only then - it decides `Failed` over `Completed` and nothing else.
    #[tokio::test]
    async fn an_evaluation_nothing_blocks_settles() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([flag("blocked", false)])
            .append_query_results([vec![eval.clone()]])
            .append_query_results([flag("failed", false)])
            .append_query_results([Vec::<MEvaluationMessage>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                3
            ])
            .append_query_results([vec![eval.clone()]])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        // The settle still spawns work that holds a context clone: drop alone
        // leaves the pool handle shared and the log unreadable.
        crate::test_ctx::settle(ctx).await;

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log.iter().any(|s| s.contains(r#"UPDATE \"evaluation\""#)),
            "the evaluation settles once nothing blocks it: {log:?}"
        );
    }

    /// An anchor still blocking stops the pass at the one read. The emitter asks
    /// this on every terminal transition, so the blocked answer must cost a single
    /// `EXISTS` and must not read the anchor set or the evaluation row.
    #[tokio::test]
    async fn a_blocked_evaluation_costs_one_read() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([flag("blocked", true)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        drop(ctx);

        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 1, "one EXISTS and nothing else: {log:?}");
        assert!(log[0].contains("SELECT EXISTS"), "{log:?}");
    }

    /// A reply with no row is an error, not "nothing blocks": settling an
    /// evaluation whose builds are still running is the dead zone this whole
    /// module exists to close.
    #[tokio::test]
    async fn a_missing_reply_does_not_read_as_settled() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let (ctx, _pool) = crate::test_ctx::ctx(db).await;
        assert!(check_evaluation_done(&ctx, eval.id).await.is_err());
    }
}
