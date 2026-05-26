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
//!
//! `Workers { connected_workers: 0 }` parks: triggered when the project's
//! organisation had no active `eval`-capable worker registration. Caller
//! (`orgs/workers.rs::{post,patch}_org_worker`) invokes
//! [`unpark_no_workers_for_org`] when a registration is created or its
//! `active`/`enable_eval` flags transition to `true`.

use crate::db::org_has_eval_capable_worker_registration;
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
    unpark_for_org(db, organization, |r| matches!(r, WaitingReason::NoCache)).await
}

/// Flip every evaluation parked with `WaitingReason::Workers { connected_workers: 0 }`
/// for projects in `organization` back to `Queued`. The zero-workers shape
/// is what `park_if_no_workers` writes when the org has no active
/// `eval`-capable worker registration at all; other `Workers { .. }` parks
/// (capability mismatch, transient runtime stall) are owned by the
/// build-dispatch reconciler and are left alone.
///
/// No-op when the organisation still has no active `eval`-capable worker
/// registration - callers in the worker endpoints invoke this unconditionally
/// after any registration touch, and this guard prevents a churn of
/// re-queue → reconciler re-park when nothing actionable changed.
pub async fn unpark_no_workers_for_org<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    if !org_has_eval_capable_worker_registration(db, organization).await? {
        return Ok(Vec::new());
    }
    unpark_for_org(db, organization, |r| {
        matches!(
            r,
            WaitingReason::Workers {
                connected_workers: 0,
                ..
            }
        )
    })
    .await
}

async fn unpark_for_org<C: ConnectionTrait, F: Fn(&WaitingReason) -> bool>(
    db: &C,
    organization: OrganizationId,
    matches_reason: F,
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
                .is_some_and(|r| matches_reason(&r))
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

/// Transition a single evaluation parked in `Waiting + Approval` back to
/// `Queued` while overriding its `wildcard` column. Same guards as
/// [`unpark_approval`]; on success, the same row update writes both the
/// status flip and the new wildcard so the dispatcher reads a consistent
/// row when it next polls.
pub async fn unpark_approval_with_wildcard(
    db: &impl ConnectionTrait,
    evaluation_id: EvaluationId,
    wildcard: &str,
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
    ae.wildcard = Set(wildcard.to_string());
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
            .is_some_and(
                |r| matches!(r, WaitingReason::Approval { pr_number: n, .. } if n == pr_number),
            )
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
            check_run_ids: None,
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

    #[tokio::test]
    async fn unpark_approval_with_wildcard_overrides_wildcard_and_requeues() {
        let mut parked = waiting_eval(WaitingReason::approval(7, "octocat"));
        parked.wildcard = "*".into();

        let mut requeued = parked.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;
        requeued.wildcard = "packages.x86_64-linux.foo".into();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![parked.clone()]])
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_approval_with_wildcard(&db, parked.id, "packages.x86_64-linux.foo")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.status, EvaluationStatus::Queued);
        assert!(out.waiting_reason.is_none());
        assert_eq!(out.wildcard, "packages.x86_64-linux.foo");
    }

    #[tokio::test]
    async fn unpark_approval_with_wildcard_no_op_for_non_approval_reason() {
        let parked = waiting_eval(WaitingReason::NoCache);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![parked.clone()]])
            .into_connection();
        assert!(
            unpark_approval_with_wildcard(&db, parked.id, "packages.*.*")
                .await
                .unwrap()
                .is_none()
        );
    }

    fn make_project(org: OrganizationId) -> entity::project::Model {
        entity::project::Model {
            id: ProjectId::now_v7(),
            organization: org,
            name: "p".into(),
            active: true,
            display_name: String::new(),
            description: String::new(),
            repository: String::new(),
            wildcard: "*".into(),
            last_evaluation: None,
            last_check_at: NaiveDateTime::default(),
            force_evaluation: false,
            created_by: crate::types::ids::UserId::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
            keep_evaluations: 10,
            concurrency: 3,
            sign_cache: true,
        }
    }

    fn eval_capable_registration() -> entity::worker_registration::Model {
        entity::worker_registration::Model {
            id: crate::types::ids::WorkerRegistrationId::now_v7(),
            peer_id: OrganizationId::nil(),
            worker_id: "00000000-0000-4000-8000-000000000001".into(),
            token_hash: String::new(),
            managed: false,
            url: None,
            active: true,
            enable_fetch: true,
            enable_eval: true,
            enable_build: true,
            display_name: String::new(),
            created_by: Some(crate::types::ids::UserId::nil()),
            created_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn unpark_no_workers_requeues_zero_workers_park_and_skips_other_workers_parks() {
        let org = OrganizationId::now_v7();
        let project = make_project(org);

        let stranded = {
            let mut e = waiting_eval(WaitingReason::workers(Vec::new(), 0, Vec::new()));
            e.project = Some(project.id);
            e
        };
        // A Workers park with connected_workers > 0 represents a capability
        // mismatch the runtime reconciler manages; the registration unpark
        // path must leave it alone.
        let capability_mismatch = {
            let mut e = waiting_eval(WaitingReason::workers(
                Vec::new(),
                1,
                vec!["aarch64-linux".into()],
            ));
            e.project = Some(project.id);
            e
        };

        let mut requeued = stranded.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Gate: org has an eval-capable registration → continue
            .append_query_results([vec![eval_capable_registration()]])
            // Fetch org's projects
            .append_query_results([vec![project.clone()]])
            // Fetch Waiting evals across those projects
            .append_query_results([vec![stranded.clone(), capability_mismatch.clone()]])
            // Update the one matching row → only `stranded` is touched
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_no_workers_for_org(&db, org).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, stranded.id);
        assert_eq!(out[0].status, EvaluationStatus::Queued);
        assert!(out[0].waiting_reason.is_none());
    }

    #[tokio::test]
    async fn unpark_no_workers_is_noop_when_no_eval_capable_registration() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Gate query: no eval-capable registration → short-circuit
            .append_query_results([Vec::<entity::worker_registration::Model>::new()])
            .into_connection();
        let out = unpark_no_workers_for_org(&db, OrganizationId::now_v7())
            .await
            .unwrap();
        assert!(out.is_empty());
    }
}
