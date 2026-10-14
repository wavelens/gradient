/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `abort_evaluation` is only marking the evaluation `Aborted`. The caller must call
//! `Scheduler::stop_aborted_evaluation`, which also aborts unneeded shared builds after
//! `AbortKind::Hard`. The in-flight builds keep running and fill the cache after `AbortKind::Soft`.

use gradient_db::lookup::by_id;
use gradient_db::status::change_evaluation_status;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::ConnectionTrait;
use sea_orm::sea_query::Expr;
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    Hard,
    Soft,
}

pub async fn abort_evaluation<C: ConnectionTrait>(
    db: &C,
    eval_id: EvaluationId,
    kind: AbortKind,
) -> Result<bool, sea_orm::DbErr> {
    let Some(eval) = by_id::<EEvaluation, _>(db, eval_id).await? else {
        return Ok(false);
    };
    if !eval.status.is_active() {
        return Ok(false);
    }

    let finished = |update: sea_orm::UpdateMany<EEvaluation>| {
        update
            .col_expr(CEvaluation::FinishedAt, Expr::value(now()))
            .col_expr(
                CEvaluation::WaitingReason,
                Expr::value(None::<serde_json::Value>),
            )
    };
    if change_evaluation_status(db, eval_id, EvaluationStatus::ABORT, finished)
        .await?
        .is_none()
    {
        return Ok(false);
    }

    debug!(evaluation_id = %eval_id, ?kind, "evaluation marked aborted");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::MockExecResult;

    fn make_eval(status: EvaluationStatus) -> gradient_entity::evaluation::Model {
        gradient_entity::evaluation::Model {
            id: EvaluationId::now_v7(),
            commit: CommitId::nil(),
            wildcard: "*".into(),
            status,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn abort_terminal_eval_is_noop() {
        let eval = make_eval(EvaluationStatus::Completed);
        let db = mock_db()
            .append_query_results([vec![eval.clone()]])
            .into_connection();
        let aborted = abort_evaluation(&db, eval.id, AbortKind::Hard)
            .await
            .unwrap();
        assert!(!aborted, "a terminal evaluation is left alone");
    }

    #[tokio::test]
    async fn soft_abort_marks_only_eval() {
        let eval = make_eval(EvaluationStatus::Building);
        let db = mock_db()
            .append_query_results([vec![eval.clone()]])
            .append_query_results([vec![eval.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let aborted = abort_evaluation(&db, eval.id, AbortKind::Soft)
            .await
            .unwrap();
        assert!(aborted, "an active evaluation is marked aborted");
    }
}
