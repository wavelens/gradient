/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Aborting an in-flight evaluation.
//!
//! - `AbortKind::Hard` marks the evaluation `Aborted` and aborts every active
//!   `derivation_build` anchor this evaluation needs that no other live
//!   evaluation also needs. Anchors still wanted by another non-terminal
//!   evaluation keep running. The scheduler drops the in-memory job entries via
//!   `Scheduler::cancel_evaluation_jobs` once this returns; that lives in the
//!   scheduler crate, not here, since the abort helper is DB-only.
//! - `AbortKind::Soft` marks only the evaluation. In-flight builds keep running
//!   and their outputs land in the cache for the next eval to reuse.

use gradient_types::*;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    Hard,
    Soft,
}

pub async fn abort_evaluation<C: ConnectionTrait>(
    db: &C,
    eval_id: EvaluationId,
    kind: AbortKind,
) -> Result<Vec<DerivationBuildId>, sea_orm::DbErr> {
    let eval = EEvaluation::find_by_id(eval_id).one(db).await?;
    let Some(eval) = eval else {
        return Ok(Vec::new());
    };
    if !eval.status.is_active() {
        return Ok(Vec::new());
    }

    let mut active: AEvaluation = eval.clone().into();
    active.status = Set(EvaluationStatus::Aborted);
    active.updated_at = Set(gradient_types::now());
    active.update(db).await?;

    if kind == AbortKind::Hard {
        return abort_eval_anchors(db, eval_id).await;
    }

    Ok(Vec::new())
}

/// Abort the active anchors this evaluation needs that no other live evaluation
/// still needs, returning the aborted [`DerivationBuildId`]s.
async fn abort_eval_anchors<C: ConnectionTrait>(
    db: &C,
    eval_id: EvaluationId,
) -> Result<Vec<DerivationBuildId>, sea_orm::DbErr> {
    let anchor_ids: Vec<DerivationBuildId> = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(eval_id))
        .all(db)
        .await?
        .into_iter()
        .map(|j| j.derivation_build)
        .collect();
    if anchor_ids.is_empty() {
        return Ok(Vec::new());
    }

    let active = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(anchor_ids))
        .filter(CDerivationBuild::Status.is_in([
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(db)
        .await?;
    if active.is_empty() {
        return Ok(Vec::new());
    }

    let active_ids: Vec<DerivationBuildId> = active.iter().map(|a| a.id).collect();
    let shared = shared_anchor_ids(db, eval_id, &active_ids).await?;

    let mut aborted = Vec::new();
    for anchor in active {
        if shared.contains(&anchor.id) {
            continue;
        }

        aborted.push(anchor.id);
        let mut ab: ADerivationBuild = anchor.into();
        ab.status = Set(BuildStatus::Aborted);
        ab.updated_at = Set(gradient_types::now());
        ab.update(db).await?;
    }

    Ok(aborted)
}

/// Of `anchor_ids`, those a non-terminal evaluation other than `this_eval` still
/// needs via its own `build_job`. Those anchors must keep running.
async fn shared_anchor_ids<C: ConnectionTrait>(
    db: &C,
    this_eval: EvaluationId,
    anchor_ids: &[DerivationBuildId],
) -> Result<HashSet<DerivationBuildId>, sea_orm::DbErr> {
    let other_jobs = EBuildJob::find()
        .filter(CBuildJob::DerivationBuild.is_in(anchor_ids.to_vec()))
        .filter(CBuildJob::Evaluation.ne(this_eval))
        .all(db)
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
    let live: HashSet<EvaluationId> = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(other_eval_ids))
        .all(db)
        .await?
        .into_iter()
        .filter(|e| e.status.is_active())
        .map(|e| e.id)
        .collect();

    Ok(other_jobs
        .into_iter()
        .filter(|j| live.contains(&j.evaluation))
        .map(|j| j.derivation_build)
        .collect())
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

    fn make_job(eval_id: EvaluationId, anchor: DerivationBuildId) -> gradient_entity::build_job::Model {
        gradient_entity::build_job::Model {
            id: BuildJobId::now_v7(),
            evaluation: eval_id,
            derivation: DerivationId::nil(),
            derivation_build: anchor,
            ..Default::default()
        }
    }

    fn make_anchor(status: BuildStatus) -> gradient_entity::derivation_build::Model {
        gradient_entity::derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: DerivationId::nil(),
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
        let ids = abort_evaluation(&db, eval.id, AbortKind::Hard)
            .await
            .unwrap();
        assert!(ids.is_empty());
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
        let ids = abort_evaluation(&db, eval.id, AbortKind::Soft)
            .await
            .unwrap();
        assert!(ids.is_empty(), "soft abort returns no anchor IDs");
    }

    #[tokio::test]
    async fn hard_abort_marks_unshared_active_anchors() {
        let eval = make_eval(EvaluationStatus::Building);
        let active_anchor = make_anchor(BuildStatus::Building);
        let done_anchor = make_anchor(BuildStatus::Completed);
        let active_anchor_id = active_anchor.id;
        let job_active = make_job(eval.id, active_anchor.id);
        let job_done = make_job(eval.id, done_anchor.id);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // initial eval fetch
            .append_query_results([vec![eval.clone()]])
            // eval update read-back
            .append_query_results([vec![eval.clone()]])
            // eval exec
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            // build_job anchor ids for the eval
            .append_query_results([vec![job_active.clone(), job_done.clone()]])
            // active anchors (only the Building one is returned by the status filter)
            .append_query_results([vec![active_anchor.clone()]])
            // shared lookup: no other eval references the anchor
            .append_query_results([Vec::<gradient_entity::build_job::Model>::new()])
            // anchor refetch for update
            .append_query_results([vec![active_anchor.clone()]])
            // anchor exec
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let ids = abort_evaluation(&db, eval.id, AbortKind::Hard)
            .await
            .unwrap();
        assert_eq!(
            ids,
            vec![active_anchor_id],
            "hard abort returns anchors it marked Aborted"
        );
    }
}
