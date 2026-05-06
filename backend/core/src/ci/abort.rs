/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Aborting an in-flight evaluation.
//!
//! - `AbortKind::Hard` marks the evaluation `Aborted` and every non-terminal
//!   build under it `Aborted`. The scheduler is expected to drop the
//!   in-memory job entries via `Scheduler::cancel_evaluation_jobs` once this
//!   helper returns Ok — that lives in the scheduler crate, not here, since
//!   the abort helper is DB-only.
//! - `AbortKind::Soft` marks only the evaluation. In-flight builds keep
//!   running and their outputs land in the cache for the next eval to reuse.

use crate::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    Hard,
    Soft,
}

pub async fn abort_evaluation<C: ConnectionTrait>(
    db: &C,
    eval_id: EvaluationId,
    kind: AbortKind,
) -> Result<Vec<BuildId>, sea_orm::DbErr> {
    let eval = EEvaluation::find_by_id(eval_id).one(db).await?;
    let Some(eval) = eval else {
        return Ok(Vec::new());
    };
    if !eval.status.is_active() {
        return Ok(Vec::new());
    }

    let mut active: AEvaluation = eval.clone().into();
    active.status = Set(EvaluationStatus::Aborted);
    active.updated_at = Set(crate::types::now());
    active.update(db).await?;

    if kind == AbortKind::Hard {
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(eval_id))
            .all(db)
            .await?;
        let mut aborted_ids = Vec::new();
        for b in builds {
            if matches!(
                b.status,
                BuildStatus::Completed
                    | BuildStatus::Substituted
                    | BuildStatus::Failed
                    | BuildStatus::Aborted
            ) {
                continue;
            }
            aborted_ids.push(b.id);
            let mut ab: ABuild = b.into();
            ab.status = Set(BuildStatus::Aborted);
            ab.updated_at = Set(crate::types::now());
            ab.update(db).await?;
        }
        return Ok(aborted_ids);
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn make_eval(status: EvaluationStatus) -> entity::evaluation::Model {
        entity::evaluation::Model {
            id: EvaluationId::now_v7(),
            project: None,
            repository: "".into(),
            commit: CommitId::nil(),
            wildcard: "*".into(),
            status,
            previous: None,
            next: None,
            created_at: chrono::NaiveDateTime::default(),
            updated_at: chrono::NaiveDateTime::default(),
            flake_source: None,
            repo_check_id: None,
            waiting_reason: None,
            trigger: None,
            concurrent: false,
        }
    }

    fn make_build(eval_id: EvaluationId, status: BuildStatus) -> entity::build::Model {
        entity::build::Model {
            id: BuildId::now_v7(),
            evaluation: eval_id,
            derivation: DerivationId::nil(),
            status,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached: false,
            created_at: chrono::NaiveDateTime::default(),
            updated_at: chrono::NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn abort_terminal_eval_is_noop() {
        let eval = make_eval(EvaluationStatus::Completed);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval.clone()]])
            .into_connection();
        let ids = abort_evaluation(&db, eval.id, AbortKind::Hard).await.unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn soft_abort_marks_only_eval() {
        let eval = make_eval(EvaluationStatus::Building);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // initial fetch
            .append_query_results([vec![eval.clone()]])
            // refetch for ActiveModel update
            .append_query_results([vec![eval.clone()]])
            // exec result for update
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();
        let ids = abort_evaluation(&db, eval.id, AbortKind::Soft).await.unwrap();
        assert!(ids.is_empty(), "soft abort returns no build IDs");
    }

    #[tokio::test]
    async fn hard_abort_marks_active_builds() {
        let eval = make_eval(EvaluationStatus::Building);
        let active_build = make_build(eval.id, BuildStatus::Building);
        let done_build = make_build(eval.id, BuildStatus::Completed);
        let active_build_id = active_build.id;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // initial eval fetch
            .append_query_results([vec![eval.clone()]])
            // eval update read-back
            .append_query_results([vec![eval.clone()]])
            // eval exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            // builds list
            .append_query_results([vec![active_build.clone(), done_build.clone()]])
            // active build refetch
            .append_query_results([vec![active_build.clone()]])
            // active build exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();
        let ids = abort_evaluation(&db, eval.id, AbortKind::Hard).await.unwrap();
        assert_eq!(ids, vec![active_build_id], "hard abort returns IDs of builds it marked Aborted");
    }
}
