/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Aborting an in-flight evaluation.
//!
//! Both kinds mark the evaluation `Aborted` in the trigger's transaction, and
//! that is all this helper does. After a [`AbortKind::Hard`] the caller asks the
//! graph actor to abort every anchor no other live evaluation still needs, then
//! drops the in-memory job entries via `Scheduler::cancel_evaluation_jobs`.
//! After a [`AbortKind::Soft`] the in-flight builds keep running and their
//! outputs land in the cache for the next evaluation to reuse.

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait};
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    Hard,
    Soft,
}

/// Marks `eval_id` aborted, reporting whether this call was the one that did it.
pub async fn abort_evaluation<C: ConnectionTrait>(
    db: &C,
    eval_id: EvaluationId,
    kind: AbortKind,
) -> Result<bool, sea_orm::DbErr> {
    let Some(eval) = EEvaluation::find_by_id(eval_id).one(db).await? else {
        return Ok(false);
    };
    if !eval.status.is_active() {
        return Ok(false);
    }

    let mut active: AEvaluation = eval.into();
    active.status = Set(EvaluationStatus::Aborted);
    active.updated_at = Set(gradient_types::now());
    active.update(db).await?;

    debug!(evaluation_id = %eval_id, ?kind, "evaluation marked aborted");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

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
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
