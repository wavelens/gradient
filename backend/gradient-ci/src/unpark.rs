/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Side-effect helpers that flip parked evaluations back to `Queued` once
//! the external condition they were waiting on clears.
//!
//! `NoCache` parks: triggered when the task's project had no writable
//! cache subscription. Caller (`projects/settings.rs::subscribe_cache`) invokes
//! [`unpark_no_cache_for_project`] right after inserting the subscription row;
//! the caller is also responsible for re-emitting the `Pending` CI status
//! for each unparked evaluation.
//!
//! `Workers { connected_workers: 0 }` parks: triggered when the task's
//! project had no active `eval`-capable worker registration. Caller
//! (`projects/workers.rs::{post,patch}_project_worker`) invokes
//! [`unpark_no_workers_for_project`] when a registration is created or its
//! `active`/`enable_eval` flags transition to `true`.

use gradient_db::project_has_eval_capable_worker_registration;
use gradient_types::ids::ProjectId;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;

use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

/// Flip every evaluation parked with `WaitingReason::NoCache` for tasks in
/// `project` back to `Queued`. Returns the updated rows so the caller
/// can re-emit pending CI checks.
pub async fn unpark_no_cache_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    unpark_for_project(db, project, |r| matches!(r, WaitingReason::NoCache)).await
}

/// Flip evaluations parked with `WaitingReason::CacheStorageFull` for tasks
/// in `project` back to `Queued`, but only when the project actually has
/// storage headroom again. The guard prevents a churn of re-queue → re-park
/// when nothing actionable changed (mirrors `unpark_no_workers_for_project`).
pub async fn unpark_storage_full_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_max_storage_gb: i32,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    if gradient_db::project_caches_all_full(db, project, instance_max_storage_gb).await? {
        return Ok(Vec::new());
    }
    unpark_for_project(db, project, |r| {
        matches!(r, WaitingReason::CacheStorageFull)
    })
    .await
}

/// Scan every project with a `CacheStorageFull`-parked evaluation and unpark those
/// that now have headroom. Used by the background cleanup pass after NARs are
/// freed, where there is no single triggering project.
pub async fn unpark_storage_full_all<C: ConnectionTrait>(
    db: &C,
    instance_max_storage_gb: i32,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    let parked = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(db)
        .await?;

    let mut projects: Vec<ProjectId> = Vec::new();
    for eval in &parked {
        let is_storage = eval
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json)
            .is_some_and(|r| matches!(r, WaitingReason::CacheStorageFull));
        if !is_storage {
            continue;
        }
        let Some(task_id) = eval.task else {
            continue;
        };
        if let Some(task) = ETask::find_by_id(task_id).one(db).await?
            && !projects.contains(&task.project)
        {
            projects.push(task.project);
        }
    }

    let mut unparked = Vec::new();
    for project in projects {
        unparked
            .extend(unpark_storage_full_for_project(db, project, instance_max_storage_gb).await?);
    }
    Ok(unparked)
}

/// Flip every evaluation parked with `WaitingReason::Workers { connected_workers: 0 }`
/// for tasks in `project` back to `Queued`. The zero-workers shape
/// is what `park_if_no_workers` writes when the project has no active
/// `eval`-capable worker registration at all; other `Workers { .. }` parks
/// (capability mismatch, transient runtime stall) are owned by the
/// build-dispatch reconciler and are left alone.
///
/// No-op when the project still has no active `eval`-capable worker
/// registration - callers in the worker endpoints invoke this unconditionally
/// after any registration touch, and this guard prevents a churn of
/// re-queue → reconciler re-park when nothing actionable changed.
pub async fn unpark_no_workers_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    if !project_has_eval_capable_worker_registration(db, project).await? {
        return Ok(Vec::new());
    }
    unpark_for_project(db, project, |r| {
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

async fn unpark_for_project<C: ConnectionTrait, F: Fn(&WaitingReason) -> bool>(
    db: &C,
    project: ProjectId,
    matches_reason: F,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    let task_ids: Vec<TaskId> = ETask::find()
        .filter(CTask::Project.eq(project))
        .all(db)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();

    if task_ids.is_empty() {
        return Ok(Vec::new());
    }

    let parked = gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Task.is_in(chunk))
            .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
            .all(db)
            .await
    })
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
        ae.updated_at = Set(gradient_types::now());
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
    ae.updated_at = Set(gradient_types::now());
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
    ae.updated_at = Set(gradient_types::now());
    Ok(Some(ae.update(db).await?))
}

/// Stamp `evaluation.source_comment` with the JSON payload describing the
/// PR comment that prompted this run. Used by the `/gradient run` /
/// `/gradient approve` unpark path so the terminal-status reaction lands on
/// the maintainer's comment, not on whatever original webhook (if any) the
/// row was created from.
pub async fn set_evaluation_source_comment(
    db: &impl ConnectionTrait,
    evaluation_id: EvaluationId,
    source_comment: serde_json::Value,
) -> Result<(), sea_orm::DbErr> {
    let Some(eval) = EEvaluation::find_by_id(evaluation_id).one(db).await? else {
        return Ok(());
    };
    let mut ae: AEvaluation = eval.into();
    ae.source_comment = Set(Some(source_comment));
    ae.updated_at = Set(gradient_types::now());
    ae.update(db).await?;
    Ok(())
}

/// Find the evaluation that is parked in `Waiting + Approval` for the given
/// task + PR number combination. Used by the comment-based unpark path
/// where the webhook only carries the PR number, not the eval id.
pub async fn find_approval_gated_eval(
    db: &impl ConnectionTrait,
    task: TaskId,
    pr_number: u64,
) -> Result<Option<MEvaluation>, sea_orm::DbErr> {
    let parked = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task))
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
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn waiting_eval(reason: WaitingReason) -> MEvaluation {
        gradient_entity::evaluation::Model {
            id: EvaluationId::now_v7(),
            task: Some(TaskId::now_v7()),
            commit: CommitId::now_v7(),
            wildcard: "*".into(),
            status: EvaluationStatus::Waiting,
            waiting_reason: Some(reason.to_json()),
            ..Default::default()
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

    fn make_task(project: ProjectId) -> gradient_entity::task::Model {
        gradient_entity::task::Model {
            id: TaskId::now_v7(),
            project: project,
            name: "p".into(),
            active: true,
            wildcard: "*".into(),
            created_by: gradient_types::ids::UserId::nil(),
            keep_evaluations: 10,
            concurrency: ConcurrencyPolicy::Skip,
            sign_cache: true,
            ..Default::default()
        }
    }

    fn eval_capable_registration() -> gradient_entity::worker_registration::Model {
        gradient_entity::worker_registration::Model {
            id: gradient_types::ids::WorkerRegistrationId::now_v7(),
            peer_id: ProjectId::nil(),
            worker_id: "00000000-0000-4000-8000-000000000001".into(),
            active: true,
            enable_fetch: true,
            enable_eval: true,
            enable_build: true,
            created_by: Some(gradient_types::ids::UserId::nil()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn unpark_no_workers_requeues_zero_workers_park_and_skips_other_workers_parks() {
        let project = ProjectId::now_v7();
        let task = make_task(project);

        let stranded = {
            let mut e = waiting_eval(WaitingReason::workers(Vec::new(), 0, Vec::new()));
            e.task = Some(task.id);
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
            e.task = Some(task.id);
            e
        };

        let mut requeued = stranded.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Gate: project has an eval-capable registration → continue
            .append_query_results([vec![eval_capable_registration()]])
            // Fetch project's tasks
            .append_query_results([vec![task.clone()]])
            // Fetch Waiting evals across those tasks
            .append_query_results([vec![stranded.clone(), capability_mismatch.clone()]])
            // Update the one matching row → only `stranded` is touched
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_no_workers_for_project(&db, project).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, stranded.id);
        assert_eq!(out[0].status, EvaluationStatus::Queued);
        assert!(out[0].waiting_reason.is_none());
    }

    #[tokio::test]
    async fn unpark_no_workers_is_noop_when_no_eval_capable_registration() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Gate: no eval-capable registration, and no base worker enabled for
            // this project either, so the gate returns false and unpark is a noop.
            .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::project_base_worker::Model>::new()])
            .into_connection();
        let out = unpark_no_workers_for_project(&db, ProjectId::now_v7())
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn unpark_storage_full_requeues_when_headroom_returns() {
        let project = ProjectId::now_v7();
        let task = make_task(project);

        let stranded = {
            let mut e = waiting_eval(WaitingReason::CacheStorageFull);
            e.task = Some(task.id);
            e
        };
        let mut requeued = stranded.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Guard `project_caches_all_full`: no writable caches → not full.
            .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
            // unpark_for_project: project's tasks
            .append_query_results([vec![task.clone()]])
            // unpark_for_project: Waiting evals across those tasks
            .append_query_results([vec![stranded.clone()]])
            // Update the matching row → requeued
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_storage_full_for_project(&db, project, 0)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, stranded.id);
        assert_eq!(out[0].status, EvaluationStatus::Queued);
        assert!(out[0].waiting_reason.is_none());
    }
}
