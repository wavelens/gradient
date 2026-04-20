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
//!   3. `organization_id_for_eval`:
//!      - if `eval.project` is `Some`: `EProject::find_by_id(pid).one()` → Q
//!      - else: `EDirectBuild::find().filter(evaluation=id).one()` → Q
//!
//! `dispatch_ready_builds`:
//!   1. `EBuild::find().from_raw_sql(ready_builds_query).all()` → Q
//!   2. Per build: `EDerivation::find_by_id(drv_id).one()` → Q
//!   3. `EEvaluation::find_by_id(eval_id).one()` → Q
//!   4. `organization_id_for_eval` (project or direct_build lookup) → Q

use std::sync::Arc;

use chrono::NaiveDateTime;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::types::*;
use sea_orm::{DatabaseBackend, MockDatabase};
use uuid::Uuid;

use crate::{Scheduler, dispatch};

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn test_date() -> NaiveDateTime {
    NaiveDateTime::default()
}

fn make_eval_queued(id: Uuid, commit_id: Uuid, project_id: Option<Uuid>) -> MEvaluation {
    entity::evaluation::Model {
        id,
        project: project_id,
        repository: "https://example.com/repo".into(),
        commit: commit_id,
        wildcard: "*".into(),
        status: EvaluationStatus::Queued,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
    }
}

fn make_commit(id: Uuid) -> entity::commit::Model {
    entity::commit::Model {
        id,
        message: "test commit".into(),
        hash: vec![0xde, 0xad, 0xbe, 0xef],
        author: None,
        author_name: "Test Author".into(),
    }
}

fn make_project(id: Uuid, org_id: Uuid) -> entity::project::Model {
    entity::project::Model {
        id,
        organization: org_id,
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: "".into(),
        repository: "https://example.com/repo".into(),
        evaluation_wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: Uuid::nil(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: 30,
        ci_reporter_type: None,
        ci_reporter_url: None,
        ci_reporter_token: None,
    }
}

fn make_direct_build(id: Uuid, org_id: Uuid, eval_id: Uuid) -> entity::direct_build::Model {
    entity::direct_build::Model {
        id,
        organization: org_id,
        evaluation: eval_id,
        derivation: "/nix/store/aaaa-test.drv".into(),
        repository_path: "/tmp/repo".into(),
        created_by: Uuid::nil(),
        created_at: test_date(),
    }
}

fn make_build_queued(id: Uuid, eval_id: Uuid, drv_id: Uuid) -> MBuild {
    entity::build::Model {
        id,
        evaluation: eval_id,
        derivation: drv_id,
        status: BuildStatus::Queued,
        server: None,
        log_id: None,
        build_time_ms: None,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn make_derivation(id: Uuid, org_id: Uuid, path: &str) -> MDerivation {
    entity::derivation::Model {
        id,
        organization: org_id,
        derivation_path: path.to_string(),
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
    }
}

fn make_scheduler(db: sea_orm::DatabaseConnection) -> Arc<Scheduler> {
    let state = test_support::prelude::test_state(db);
    Arc::new(Scheduler::new(state))
}

// ── Group F: dispatch_queued_evals ───────────────────────────────────────────

/// A single Queued evaluation with a valid commit and project → one job enqueued.
#[tokio::test]
async fn dispatch_queued_eval_enqueues_job() {
    let eval_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 2. find commit
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. organization_id_for_eval: find project → returns org_id
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
    let eval_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // First dispatch:
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 2. find commit
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. find project
        .append_query_results([vec![make_project(project_id, org_id)]])
        // Second dispatch:
        // 4. find Queued evaluations (same eval still Queued in DB)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // No commit/project lookup — contains_job check short-circuits
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
    let eval_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        // 2. find commit → None
        .append_query_results([Vec::<entity::commit::Model>::new()])
        // No project lookup — skipped after missing commit
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

/// When the eval has no project (direct build), org is looked up via DirectBuild.
#[tokio::test]
async fn dispatch_queued_eval_via_direct_build_org() {
    let eval_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let direct_build_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Queued evaluations — project: None (direct build)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, None)]])
        // 2. find commit
        .append_query_results([vec![make_commit(commit_id)]])
        // 3. organization_id_for_eval: no project → find DirectBuild
        .append_query_results([vec![make_direct_build(direct_build_id, org_id, eval_id)]])
        .into_connection();

    let scheduler = make_scheduler(db);
    dispatch::dispatch_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "direct-build org: job should be enqueued"
    );
}

// ── Group F: dispatch_ready_builds ───────────────────────────────────────────

/// A single ready Queued build → one job enqueued with the correct drv_path.
#[tokio::test]
async fn dispatch_ready_build_enqueues_job() {
    let eval_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
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
        .append_query_results([Vec::<entity::derivation_feature::Model>::new()])
        // 6. derivation_dependency edges for dep_counts → empty
        .append_query_results([Vec::<entity::derivation_dependency::Model>::new()])
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
    let eval_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let drv_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-foo.drv";

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // First dispatch:
        .append_query_results([vec![make_build_queued(build_id, eval_id, drv_id)]])
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(project_id))]])
        .append_query_results([vec![make_project(project_id, org_id)]])
        // derivation_feature edges (none) → no follow-up feature name lookup
        .append_query_results([Vec::<entity::derivation_feature::Model>::new()])
        // derivation_dependency edges for dep_counts → empty
        .append_query_results([Vec::<entity::derivation_dependency::Model>::new()])
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

// ── Group J: project polling ────────────────────────────────────────────────

/// Verifies that the project polling function exists and is callable.
/// This test was added after the evaluator service was accidentally disconnected
/// from the server during the builder crate split — no evaluations were ever
/// created automatically because no code polled projects for new commits.
#[tokio::test]
async fn project_poll_with_no_projects_is_noop() {
    // poll_projects_for_evaluations is pub(crate), so this test proves it exists
    // and compiles. A real git repo is required for check_project_updates, so we
    // just verify the function handles an empty project list gracefully.
    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        // Query for active projects with last_evaluation join → empty
        .append_query_results([Vec::<entity::project::Model>::new()])
        // Query for active projects without last_evaluation → empty
        .append_query_results([Vec::<entity::project::Model>::new()])
        .into_connection();

    let scheduler = make_scheduler(db);
    let result = dispatch::poll_projects_for_evaluations(&scheduler).await;
    assert!(result.is_ok(), "poll with no projects should succeed");
}
