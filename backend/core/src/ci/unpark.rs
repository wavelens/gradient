/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Side-effect helpers that flip parked evaluations back to `Queued` once
//! the external condition they were waiting on clears.
//!
//! `NoCache` parks: triggered when the project's organisation had no writable
//! cache subscription. Caller (`orgs/settings.rs::subscribe_cache`) invokes
//! [`unpark_no_cache_for_org`] right after inserting the subscription row;
//! the caller is also responsible for re-emitting the `Pending` CI status
//! for each unparked evaluation.

use crate::types::ids::OrganizationId;
use crate::types::waiting_reason::WaitingReason;
use crate::types::*;

use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

/// Flip every evaluation parked with `WaitingReason::NoCache` for projects in
/// `organization` back to `Queued`. Returns the updated rows so the caller
/// can re-emit pending CI checks.
pub async fn unpark_no_cache_for_org<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    let project_ids: Vec<ProjectId> = EProject::find()
        .filter(CProject::Organization.eq(organization))
        .all(db)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();

    if project_ids.is_empty() {
        return Ok(Vec::new());
    }

    let parked = EEvaluation::find()
        .filter(CEvaluation::Project.is_in(project_ids))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(db)
        .await?;

    let candidates: Vec<MEvaluation> = parked
        .into_iter()
        .filter(|e| {
            e.waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json)
                .is_some_and(|r| matches!(r, WaitingReason::NoCache))
        })
        .collect();

    let mut unparked = Vec::with_capacity(candidates.len());
    for eval in candidates {
        let mut ae: AEvaluation = eval.into();
        ae.status = Set(EvaluationStatus::Queued);
        ae.waiting_reason = Set(None);
        ae.updated_at = Set(crate::types::now());
        unparked.push(ae.update(db).await?);
    }
    Ok(unparked)
}

/// Transition a single evaluation parked in `Waiting + Approval` back to
/// `Queued`. Returns `Ok(None)` when the row isn't parked-Approval (already
/// unparked, never parked, status drifted) so the caller can decide whether
/// to log or ignore.
pub async fn unpark_approval(
    db: &impl ConnectionTrait,
    evaluation_id: EvaluationId,
) -> Result<Option<MEvaluation>, sea_orm::DbErr> {
    let Some(eval) = EEvaluation::find_by_id(evaluation_id).one(db).await? else {
        return Ok(None);
    };
    if eval.status != EvaluationStatus::Waiting {
        return Ok(None);
    }
    let is_approval = eval
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
        .is_some_and(|r| matches!(r, WaitingReason::Approval { .. }));
    if !is_approval {
        return Ok(None);
    }
    let mut ae: AEvaluation = eval.into();
    ae.status = Set(EvaluationStatus::Queued);
    ae.waiting_reason = Set(None);
    ae.updated_at = Set(crate::types::now());
    Ok(Some(ae.update(db).await?))
}

/// Find the evaluation that is parked in `Waiting + Approval` for the given
/// project + PR number combination. Used by the comment-based unpark path
/// where the webhook only carries the PR number, not the eval id.
pub async fn find_approval_gated_eval(
    db: &impl ConnectionTrait,
    project: ProjectId,
    pr_number: u64,
) -> Result<Option<MEvaluation>, sea_orm::DbErr> {
    let parked = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(db)
        .await?;
    Ok(parked.into_iter().find(|e| {
        e.waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json)
            .is_some_and(|r| matches!(r, WaitingReason::Approval { pr_number: n, .. } if n == pr_number))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn waiting_eval(reason: WaitingReason) -> MEvaluation {
        entity::evaluation::Model {
            id: EvaluationId::now_v7(),
            project: Some(ProjectId::now_v7()),
            repository: String::new(),
            commit: CommitId::now_v7(),
            wildcard: "*".into(),
            status: EvaluationStatus::Waiting,
            previous: None,
            next: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
            flake_source: None,
            repo_check_id: None,
            waiting_reason: Some(reason.to_json()),
            trigger: None,
            concurrent: false,
        }
    }

    #[tokio::test]
    async fn unpark_approval_requeues_waiting_approval_row() {
        let parked = waiting_eval(WaitingReason::approval(7, "octocat"));
        let mut requeued = parked.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![parked.clone()]])
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_approval(&db, parked.id).await.unwrap().unwrap();
        assert_eq!(out.status, EvaluationStatus::Queued);
        assert!(out.waiting_reason.is_none());
    }

    #[tokio::test]
    async fn unpark_approval_no_op_for_non_approval_reason() {
        let parked = waiting_eval(WaitingReason::NoCache);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![parked.clone()]])
            .into_connection();
        assert!(unpark_approval(&db, parked.id).await.unwrap().is_none());
    }
}
