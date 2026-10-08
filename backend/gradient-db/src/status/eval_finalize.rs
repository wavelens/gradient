/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_status::update_evaluation_status;
use crate::DbContext;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};
use tracing::info;

/// The counters are only answering "not yet".
/// A naming and a transition in flight together can miss each other.
/// The exact reads are confirming zero instead.
pub async fn check_evaluation_done(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
) -> Result<(), DbErr> {
    if ctx.held_evaluations.holds(evaluation_id) {
        return Ok(());
    }

    let db = &ctx.worker_db;
    let Some(counters) = crate::evaluations::counters::eval_counters(db, evaluation_id).await?
    else {
        return Ok(());
    };
    if counters.active > 0 {
        return Ok(());
    }

    if crate::graph::reachability::eval_blocked(db, evaluation_id).await? {
        crate::evaluations::counters::recount_evaluations(db, &[evaluation_id]).await?;
        return Ok(());
    }

    let Some(eval) = EEvaluation::find_by_id(evaluation_id).one(db).await? else {
        return Ok(());
    };

    if !in_build_phase(&eval) {
        return Ok(());
    }

    let any_failed =
        crate::graph::reachability::eval_any_shared_build_failed(db, evaluation_id).await?;

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

    update_evaluation_status(ctx, eval, target).await?;
    Ok(())
}

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

pub async fn finalize_evals_for_derivations(
    ctx: &DbContext,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    let evaluations =
        crate::graph::reachability::evals_referencing_derivations(&ctx.worker_db, derivations)
            .await?;
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

    fn counters(active: i64, failed: i64) -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([
            ("named".to_owned(), Value::BigInt(Some(2))),
            ("active".to_owned(), Value::BigInt(Some(active))),
            ("failed".to_owned(), Value::BigInt(Some(failed))),
            ("queued".to_owned(), Value::BigInt(Some(0))),
            ("building".to_owned(), Value::BigInt(Some(0))),
        ])]
    }

    fn flag(column: &str, value: bool) -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([(
            column.to_owned(),
            Value::Bool(Some(value)),
        )])]
    }

    fn exec(n: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: n,
        }
    }

    fn building() -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            status: EvaluationStatus::Building,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn an_evaluation_nothing_blocks_settles() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([counters(0, 0)])
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
            .append_query_results([crate::test_ctx::inserted_phase_event()])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        // The settle is still spawning work holding a context clone.
        // A plain drop would leave the pool handle shared and the log unreadable.
        crate::test_ctx::settle(ctx).await;

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log.iter().any(|s| s.contains(r#"UPDATE \"evaluation\""#)),
            "the evaluation settles once nothing blocks it: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_held_evaluation_stays_open() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let _hold = ctx.held_evaluations.hold(eval.id);
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        drop(ctx);

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(log.is_empty(), "{log:?}");
    }

    #[tokio::test]
    async fn a_blocked_evaluation_costs_one_read() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([counters(1, 0)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        drop(ctx);

        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 1, "one counters read and nothing else: {log:?}");
        assert!(log[0].contains("evaluation_shared_build_delta"), "{log:?}");
    }

    #[tokio::test]
    async fn a_failed_shared_build_fails_the_evaluation() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([counters(0, 0)])
            .append_query_results([flag("blocked", false)])
            .append_query_results([vec![eval.clone()]])
            .append_query_results([flag("failed", true)])
            .append_query_results([Vec::<MEvaluationMessage>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                3
            ])
            .append_query_results([vec![eval.clone()]])
            .append_query_results([crate::test_ctx::inserted_phase_event()])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        crate::test_ctx::settle(ctx).await;

        let failed = format!("{:?}", Value::Int(Some(EvaluationStatus::Failed as i32)));
        let log = pool.into_transaction_log();
        let settled = log
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| s.sql.starts_with(r#"UPDATE "evaluation""#))
            .expect("the evaluation settles");
        assert!(
            format!("{:?}", settled.values).contains(&failed),
            "{settled:?}"
        );
    }

    #[tokio::test]
    async fn counters_the_exact_read_contradicts_are_recounted() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([counters(0, 0)])
            .append_query_results([flag("blocked", true)])
            .append_exec_results([exec(1), exec(1)])
            .into_connection();

        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
        drop(ctx);

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log[2].contains("pg_advisory_xact_lock")
                && log[3].contains("DELETE FROM evaluation_shared_build_delta"),
            "{log:?}"
        );
        assert!(
            !log.iter().any(|s| s.contains(r#"UPDATE \"evaluation\""#)),
            "a contradicted zero never settles: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_deleted_evaluation_is_nothing_to_settle() {
        let eval = building();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let (ctx, _pool) = crate::test_ctx::ctx(db).await;
        check_evaluation_done(&ctx, eval.id).await.unwrap();
    }
}
