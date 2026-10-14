/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use gradient_types::*;

pub async fn wait_active_evals_for_draining<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    let res = crate::status::evaluation_change(EvaluationStatus::WAIT_FOR_DRAINING)
        .col_expr(
            CEvaluation::WaitingReason,
            Expr::value(Some(WaitingReason::Draining.to_json())),
        )
        .col_expr(CEvaluation::UpdatedAt, Expr::value(now()))
        .exec(conn)
        .await?;

    Ok(res.rows_affected)
}

pub async fn resume_drained_evals<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    let drained: Vec<MEvaluation> = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(conn)
        .await?
        .into_iter()
        .filter(|e| {
            e.waiting_reason.as_ref().and_then(WaitingReason::from_json)
                == Some(WaitingReason::Draining)
        })
        .collect();

    let mut resumed = 0;
    for target in [EvaluationStatus::Queued, EvaluationStatus::Building] {
        let ids: Vec<EvaluationId> = drained
            .iter()
            .filter(|e| e.status_after_draining() == target)
            .map(|e| e.id)
            .collect();
        if ids.is_empty() {
            continue;
        }

        resumed += crate::status::evaluation_change(target.report())
            .col_expr(
                CEvaluation::WaitingReason,
                Expr::value(None::<serde_json::Value>),
            )
            .col_expr(CEvaluation::UpdatedAt, Expr::value(now()))
            .filter(CEvaluation::Id.is_in(ids))
            .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
            .exec(conn)
            .await?
            .rows_affected;
    }

    Ok(resumed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::Model as MEval;
    use gradient_entity::ids::{CommitId, EvaluationId};
    use gradient_fixtures::mock::mock_db;
    use sea_orm::MockExecResult;

    fn waiting_eval(reason: Option<WaitingReason>) -> MEval {
        MEval {
            id: EvaluationId::now_v7(),
            status: EvaluationStatus::Waiting,
            repository: "git+https://example.com/repo".into(),
            commit: CommitId::now_v7(),
            wildcard: "**".into(),
            waiting_reason: reason.map(|r| r.to_json()),
            created_at: now(),
            updated_at: now(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn unpark_touches_only_draining_parks() {
        let db = mock_db()
            .append_query_results([vec![
                waiting_eval(Some(WaitingReason::Draining)),
                waiting_eval(Some(WaitingReason::NoCache)),
            ]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        assert_eq!(resume_drained_evals(&db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn unpark_skips_update_when_no_draining_parks() {
        let db = mock_db()
            .append_query_results([vec![waiting_eval(Some(WaitingReason::NoCache))]])
            .into_connection();

        assert_eq!(resume_drained_evals(&db).await.unwrap(), 0);
    }
}
