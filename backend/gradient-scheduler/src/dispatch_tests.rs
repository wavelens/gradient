/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `dispatch_queued_evals` and `dispatch_ready_builds`.
//!
//! Both functions are tested with a staged `MockDatabase` and a real `Scheduler`
//! so we can assert on `scheduler.pending_job_count()` after dispatch.
//!
//! ## DB call sequences
//!
//! `dispatch_queued_evals`:
//!   1. `EEvaluation::find().filter(status=Queued).all()` → Q
//!   2. Bulk `ECommit IN (commit ids)` → Q (skipped when no untracked evals)
//!   3. Bulk sidecar `evaluation_input_update IN (...)` → Q (InputUpdate evals only)
//!   4. Bulk `evaluation_flake_input_override IN (eval ids)` → Q
//!   5. Bulk `ETask IN (task ids)` → Q (skipped when no eval has a task)
//!
//! `dispatch_ready_builds`:
//!   1. `EBuild::find().from_raw_sql(ready_builds_query).all()` → Q
//!   2. Per build: `EDerivation::find_by_id(drv_id).one()` → Q
//!   3. `EEvaluation::find_by_id(eval_id).one()` → Q
//!   4. `project_id_for_eval` (task lookup) → Q

use std::sync::Arc;

use chrono::NaiveDateTime;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{DatabaseBackend, MockDatabase};

use crate::{Scheduler, dispatch, trigger_dispatch};

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn test_date() -> NaiveDateTime {
    NaiveDateTime::default()
}

fn make_eval_queued(id: EvaluationId, commit_id: CommitId, task_id: Option<TaskId>) -> MEvaluation {
    gradient_entity::evaluation::Model {
        id,
        task: task_id,
        repository: "https://example.com/repo".into(),
        commit: commit_id,
        wildcard: "*".into(),
        status: EvaluationStatus::Queued,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn make_commit(id: CommitId) -> gradient_entity::commit::Model {
    gradient_entity::commit::Model {
        id,
        message: "test commit".into(),
        hash: vec![0xde, 0xad, 0xbe, 0xef],
        author_name: "Test Author".into(),
        ..Default::default()
    }
}

fn make_task(id: TaskId, project_id: ProjectId) -> gradient_entity::task::Model {
    gradient_entity::task::Model {
        id,
        project: project_id,
        name: "test-task".into(),
        active: true,
        display_name: "Test Task".into(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: UserId::nil(),
        created_at: test_date(),
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::Skip,
        sign_cache: true,
        ..Default::default()
    }
}

async fn make_scheduler(db: sea_orm::DatabaseConnection) -> Arc<Scheduler> {
    let state = gradient_test_support::prelude::test_state(db);
    let scheduler = Arc::new(Scheduler::new(state));
    scheduler.spawn_core(None).await.expect("core actor");
    scheduler
}

// ── Group F: dispatch_queued_evals ───────────────────────────────────────────

/// A single Queued evaluation with a valid commit and task → one job enqueued.
#[tokio::test]
async fn dispatch_queued_eval_enqueues_job() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();
    let project_id = ProjectId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        // 2. bulk commits
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. bulk flake input overrides (none)
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        // 4. bulk tasks → returns project_id
        .append_query_results([vec![make_task(task_id, project_id)]])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "expected 1 job enqueued"
    );
}

/// Calling dispatch twice for the same Queued eval does not enqueue a second job.
/// The second call sees `contains_job` = true and skips the commit/project lookup.
#[tokio::test]
async fn dispatch_queued_eval_skips_already_enqueued() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();
    let project_id = ProjectId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // First dispatch:
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        // 2. bulk commits
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. bulk flake input overrides (none)
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        // 4. bulk tasks
        .append_query_results([vec![make_task(task_id, project_id)]])
        // Second dispatch:
        // 5. find Queued evaluations (same eval still Queued in DB)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        // No bulk loads - the tracker snapshot filters out every eval
        .into_connection();

    let scheduler = make_scheduler(db).await;
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("first dispatch failed");
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("second dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "second dispatch must be a no-op"
    );
}

/// When the commit row is missing, the eval is skipped and no job is enqueued.
#[tokio::test]
async fn dispatch_queued_eval_skips_missing_commit() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        // 2. bulk commits → none found
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        // 3. bulk flake input overrides (none)
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        // 4. bulk tasks (loaded up front; the eval is skipped per-row later)
        .append_query_results([Vec::<gradient_entity::task::Model>::new()])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "missing commit: no job should be enqueued"
    );
}

/// An eval with no task is skipped (every eval must belong to a task
/// after the build-request rework removed the legacy direct-build path).
#[tokio::test]
async fn dispatch_queued_eval_without_task_is_skipped() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations - task: None
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, None)]])
        // 2. bulk commits
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. bulk flake input overrides (none)
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        // No task query - no eval carries a task id
        .into_connection();

    let scheduler = make_scheduler(db).await;
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "eval without task must not be enqueued"
    );
}

// ── Group J: trigger dispatch_once ───────────────────────────────────────────

fn make_polling_trigger(
    id: TaskTriggerId,
    task_id: TaskId,
    interval_secs: u32,
    last_fired_at: Option<NaiveDateTime>,
) -> gradient_entity::task_trigger::Model {
    gradient_entity::task_trigger::Model {
        id,
        task: task_id,
        config: serde_json::json!({ "interval_secs": interval_secs }),
        active: true,
        last_fired_at,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

/// `dispatch_once` with no active polling/time triggers is a no-op.
#[tokio::test]
async fn dispatch_once_no_triggers_is_noop() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Query for active polling/time triggers → empty
        .append_query_results([Vec::<gradient_entity::task_trigger::Model>::new()])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    let result = trigger_dispatch::dispatch_once(&scheduler).await;
    assert!(
        result.is_ok(),
        "dispatch_once with no triggers should succeed"
    );
}

/// A trigger whose `last_fired_at` is recent (within interval) must not cause
/// an evaluation - the `dispatch_once` loop skips it as not-due.
///
/// We verify this by asserting no task lookup follows the trigger query,
/// which means no evaluation creation path is entered. If the mock DB were
/// drained by a task lookup, sea-orm would panic on an empty queue.
#[tokio::test]
async fn dispatch_once_skips_trigger_within_interval() {
    let task_id = TaskId::now_v7();
    let trigger_id = TaskTriggerId::now_v7();
    let project_id = ProjectId::now_v7();

    // last_fired_at = now (0 seconds ago) - interval = 60 s → not due
    let recent = gradient_types::now();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. active polling/time triggers → one trigger, recently fired
        .append_query_results([vec![make_polling_trigger(
            trigger_id,
            task_id,
            60,
            Some(recent),
        )]])
        // 2. task lookup (batch)
        .append_query_results([vec![make_task(task_id, project_id)]])
        // No further queries expected (trigger not due)
        .into_connection();

    let scheduler = make_scheduler(db).await;
    trigger_dispatch::dispatch_once(&scheduler)
        .await
        .expect("dispatch_once should not fail");
    // No evaluation rows means no job was enqueued
    assert_eq!(scheduler.pending_job_count().await, 0);
}
