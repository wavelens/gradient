/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Instance-draining eval transitions. Enabling draining parks every in-flight
//! evaluation under [`WaitingReason::Draining`]; disabling it (or the next
//! startup) recovers those parks back to `Queued`.

use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use gradient_types::*;

/// Park every active evaluation under a `Draining` waiting reason so the
/// scheduler stops advancing it. Returns the number of evaluations parked.
pub async fn park_active_evals<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    let res = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Waiting))
        .col_expr(
            CEvaluation::WaitingReason,
            Expr::value(Some(WaitingReason::Draining.to_json())),
        )
        .col_expr(CEvaluation::UpdatedAt, Expr::value(now()))
        .filter(CEvaluation::Status.is_in([
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
        ]))
        .exec(conn)
        .await?;

    Ok(res.rows_affected)
}

/// Recover evaluations parked under `Draining` back to `Queued` (clearing the
/// reason) so normal scheduling resumes. Only `Draining` parks are touched -
/// capacity/approval parks are left to their owning hooks. Returns the count.
pub async fn unpark_draining_evals<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    let waiting = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(conn)
        .await?;

    let ids: Vec<EvaluationId> = waiting
        .iter()
        .filter(|e| {
            e.waiting_reason.as_ref().and_then(WaitingReason::from_json)
                == Some(WaitingReason::Draining)
        })
        .map(|e| e.id)
        .collect();

    if ids.is_empty() {
        return Ok(0);
    }

    let res = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, Expr::value(EvaluationStatus::Queued))
        .col_expr(
            CEvaluation::WaitingReason,
            Expr::value(None::<serde_json::Value>),
        )
        .col_expr(CEvaluation::UpdatedAt, Expr::value(now()))
        .filter(CEvaluation::Id.is_in(ids))
        .exec(conn)
        .await?;

    Ok(res.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::Model as MEval;
    use gradient_entity::ids::{CommitId, EvaluationId};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

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
    async fn park_returns_rows_affected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 4 }])
            .into_connection();

        assert_eq!(park_active_evals(&db).await.unwrap(), 4);
    }

    #[tokio::test]
    async fn unpark_touches_only_draining_parks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // SELECT Waiting evals: one Draining park, one capacity park.
            .append_query_results([vec![
                waiting_eval(Some(WaitingReason::Draining)),
                waiting_eval(Some(WaitingReason::NoCache)),
            ]])
            // UPDATE the single Draining park back to Queued.
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        assert_eq!(unpark_draining_evals(&db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn unpark_skips_update_when_no_draining_parks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![waiting_eval(Some(WaitingReason::NoCache))]])
            .into_connection();

        assert_eq!(unpark_draining_evals(&db).await.unwrap(), 0);
    }
}
