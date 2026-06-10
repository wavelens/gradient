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
//!   2. Per eval: `ECommit::find_by_id(commit_id).one()` → Q
//!   3. `organization_id_for_eval`: `EProject::find_by_id(pid).one()` → Q
//!
//! `dispatch_ready_builds`:
//!   1. `EBuild::find().from_raw_sql(ready_builds_query).all()` → Q
//!   2. Per build: `EDerivation::find_by_id(drv_id).one()` → Q
//!   3. `EEvaluation::find_by_id(eval_id).one()` → Q
//!   4. `organization_id_for_eval` (project lookup) → Q

use std::sync::Arc;

use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{DatabaseBackend, MockDatabase};

use crate::{Scheduler, dispatch, trigger_dispatch};

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn test_date() -> NaiveDateTime {
    NaiveDateTime::default()
}

fn make_eval_queued(
    id: EvaluationId,
    commit_id: CommitId,
    project_id: Option<ProjectId>,
) -> MEvaluation {
    gradient_entity::evaluation::Model {
        id,
        project: project_id,
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

fn make_project(id: ProjectId, org_id: OrganizationId) -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        id,
        organization: org_id,
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: UserId::nil(),
        created_at: test_date(),
        keep_evaluations: 30,
        concurrency: 3,
        sign_cache: true,
        ..Default::default()
    }
}

fn make_build_queued(id: BuildId, eval_id: EvaluationId, drv_id: DerivationId) -> MBuild {
    gradient_entity::build::Model {
        id,
        evaluation: eval_id,
        derivation: drv_id,
        status: BuildStatus::Queued,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn make_derivation(id: DerivationId, org_id: OrganizationId, path: &str) -> MDerivation {
    let stripped = gradient_exec::strip_nix_store_prefix(path);
    let (hash, name) = gradient_sources::parse_drv_hash_name(&stripped)
        .unwrap_or_else(|_| ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), "x".into()));
    gradient_entity::derivation::Model {
        id,
        organization: org_id,
        hash,
        name,
        architecture: "x86_64-linux".into(),
        // Pre-set so dispatch's lazy backfill skips the closure walk in these
        // strictly-ordered MockDatabase fixtures.
        closure_size: Some(0),
        created_at: test_date(),
        ..Default::default()
    }
}

fn make_scheduler(db: sea_orm::DatabaseConnection) -> Arc<Scheduler> {
    let state = gradient_test_support::prelude::test_state(db);
    Arc::new(Scheduler::new(state))
}

// ── Group F: dispatch_queued_evals ───────────────────────────────────────────

/// A single Queued evaluation with a valid commit and project → one job enqueued.
#[tokio::test]
async fn dispatch_queued_eval_enqueues_job() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let project_id = ProjectId::now_v7();
    let org_id = OrganizationId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 2. find commit
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::evaluation_flake_input_override::Model>::new()])
        // 4. organization_id_for_eval: find project → returns org_id
        .append_query_results([vec![make_project(project_id, org_id)]])
        .into_connection();

    let scheduler = make_scheduler(db);
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
/// The second call sees `contains_job` = true and skips the commit/org lookup.
#[tokio::test]
async fn dispatch_queued_eval_skips_already_enqueued() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let project_id = ProjectId::now_v7();
    let org_id = OrganizationId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // First dispatch:
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 2. find commit
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::evaluation_flake_input_override::Model>::new()])
        // 4. find project
        .append_query_results([vec![make_project(project_id, org_id)]])
        // Second dispatch:
        // 5. find Queued evaluations (same eval still Queued in DB)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // No commit/project lookup - contains_job check short-circuits
        .into_connection();

    let scheduler = make_scheduler(db);
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
    let project_id = ProjectId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 2. find commit → None
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        // No project lookup - skipped after missing commit
        .into_connection();

    let scheduler = make_scheduler(db);
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "missing commit: no job should be enqueued"
    );
}

/// An eval with no project is skipped (every eval must belong to a project
/// after the build-request rework removed the legacy direct-build path).
#[tokio::test]
async fn dispatch_queued_eval_without_project_is_skipped() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations - project: None
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, None)]])
        // 2. find commit
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. snapshot flake input overrides (none) - runs even when project: None
        .append_query_results([Vec::<gradient_entity::evaluation_flake_input_override::Model>::new()])
        // No project lookup - organization_id_for_eval bails on None project
        .into_connection();

    let scheduler = make_scheduler(db);
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "eval without project must not be enqueued"
    );
}

// ── Group F: dispatch_ready_builds ───────────────────────────────────────────

/// A single ready Queued build → one job enqueued with the correct drv_path.
#[tokio::test]
async fn dispatch_ready_build_enqueues_job() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let project_id = ProjectId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let drv_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. raw SQL: ready builds → [build]
        .append_query_results([vec![make_build_queued(build_id, eval_id, drv_id)]])
        // 2. find derivation
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 3. find evaluation
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 4. organization_id_for_eval: find project
        .append_query_results([vec![make_project(project_id, org_id)]])
        // 5. derivation_feature edges for required_features lookup → empty
        .append_query_results([Vec::<gradient_entity::derivation_feature::Model>::new()])
        // 6. derivation_dependency edges for dep_counts → empty
        .append_query_results([Vec::<gradient_entity::derivation_dependency::Model>::new()])
        .into_connection();

    let scheduler = make_scheduler(db);
    dispatch::dispatch_ready_builds(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "expected 1 build job enqueued"
    );
}

/// Calling dispatch_ready_builds twice for the same build does not enqueue a second job.
#[tokio::test]
async fn dispatch_ready_build_skips_already_enqueued() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let project_id = ProjectId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let drv_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foo.drv";

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // First dispatch:
        .append_query_results([vec![make_build_queued(build_id, eval_id, drv_id)]])
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        .append_query_results([vec![make_project(project_id, org_id)]])
        // derivation_feature edges (none) → no follow-up feature name lookup
        .append_query_results([Vec::<gradient_entity::derivation_feature::Model>::new()])
        // derivation_dependency edges for dep_counts → empty
        .append_query_results([Vec::<gradient_entity::derivation_dependency::Model>::new()])
        // Second dispatch:
        // raw SQL query returns the same build; contains_job → true → skips lookups
        .append_query_results([vec![make_build_queued(build_id, eval_id, drv_id)]])
        // No derivation / eval / project / feature lookups (contains_job = true)
        .into_connection();

    let scheduler = make_scheduler(db);
    dispatch::dispatch_ready_builds(&scheduler)
        .await
        .expect("first dispatch failed");
    dispatch::dispatch_ready_builds(&scheduler)
        .await
        .expect("second dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "second dispatch must be a no-op"
    );
}

// ── Group J: trigger dispatch_once ───────────────────────────────────────────

fn make_polling_trigger(
    id: ProjectTriggerId,
    project_id: ProjectId,
    interval_secs: u32,
    last_fired_at: Option<NaiveDateTime>,
) -> gradient_entity::project_trigger::Model {
    gradient_entity::project_trigger::Model {
        id,
        project: project_id,
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
        .append_query_results([Vec::<gradient_entity::project_trigger::Model>::new()])
        .into_connection();

    let scheduler = make_scheduler(db);
    let result = trigger_dispatch::dispatch_once(&scheduler).await;
    assert!(
        result.is_ok(),
        "dispatch_once with no triggers should succeed"
    );
}

/// A trigger whose `last_fired_at` is recent (within interval) must not cause
/// an evaluation - the `dispatch_once` loop skips it as not-due.
///
/// We verify this by asserting no project lookup follows the trigger query,
/// which means no evaluation creation path is entered. If the mock DB were
/// drained by a project lookup, sea-orm would panic on an empty queue.
#[tokio::test]
async fn dispatch_once_skips_trigger_within_interval() {
    let project_id = ProjectId::now_v7();
    let trigger_id = ProjectTriggerId::now_v7();
    let org_id = OrganizationId::now_v7();

    // last_fired_at = now (0 seconds ago) - interval = 60 s → not due
    let recent = gradient_types::now();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. active polling/time triggers → one trigger, recently fired
        .append_query_results([vec![make_polling_trigger(
            trigger_id,
            project_id,
            60,
            Some(recent),
        )]])
        // 2. project lookup (batch)
        .append_query_results([vec![make_project(project_id, org_id)]])
        // No further queries expected (trigger not due)
        .into_connection();

    let scheduler = make_scheduler(db);
    trigger_dispatch::dispatch_once(&scheduler)
        .await
        .expect("dispatch_once should not fail");
    // No evaluation rows means no job was enqueued
    assert_eq!(scheduler.pending_job_count().await, 0);
}

// ── Group K: via / follower behaviour ────────────────────────────────────────

/// A follower build (`via IS NOT NULL`) is filtered out by the SQL gate, so
/// the dispatcher never sees it. Issue #175.
#[tokio::test]
async fn dispatch_skips_follower_builds() {
    // The ready-builds SQL has `AND b.via IS NULL`, so a follower row never
    // makes it into the result set. We model that by returning an empty list
    // from the raw SQL - the test asserts the dispatcher then enqueues nothing.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<MBuild>::new()])
        .into_connection();

    let scheduler = make_scheduler(db);
    dispatch::dispatch_ready_builds(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "follower builds must not be enqueued"
    );
}
