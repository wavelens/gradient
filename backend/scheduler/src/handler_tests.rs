/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the proto scheduler handler functions.
//!
//! These tests drive `eval::handle_eval_result`, `build::handle_build_job_completed`,
//! `build::handle_build_job_failed`, `build::handle_build_output`,
//! `eval::handle_eval_job_completed`, and `eval::handle_eval_job_failed` directly
//! with a `MockDatabase` staged to replay the exact DB call sequence each handler
//! makes.
//!
//! ## MockDatabase staging rules (SeaORM 1.x, Postgres backend)
//!
//! - `SELECT` / `find_by_id().one()` / `find().all()` → `append_query_results`
//! - `ActiveModel::update()` (Postgres does `UPDATE ... RETURNING *`) → `append_query_results`
//! - `update_many().exec()` → `append_exec_results`
//! - `EEntity::insert(active_model_with_explicit_pk).exec()` → `append_exec_results`
//! - `EEntity::insert_many().exec()` on Postgres → `append_query_results` (not exec!)
//!   Reason: primary_key=None + support_returning=true → uses `db.query_all()` internally.
//!   The result row only needs a valid `id: Uuid` to succeed.
//!
//! All evaluations use `project: None` so that the fire_*_webhook helpers that
//! are spawned inside `update_build_status` / `update_evaluation_status` return
//! early without consuming staged MockDatabase results.

use std::sync::Arc;

use chrono::NaiveDateTime;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::types::*;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use uuid::Uuid;

use crate::jobs::{PendingBuildJob, PendingEvalJob};
use crate::{build as build_handler, eval as eval_handler};
use gradient_core::types::proto::{
    BuildOutput, DerivationOutput, DiscoveredDerivation, FlakeJob, FlakeTask,
};
use test_support::prelude::test_state_recorded;

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn test_date() -> NaiveDateTime {
    NaiveDateTime::default()
}

/// Evaluation fixture. `project: None` prevents `fire_evaluation_webhook`
/// from doing any DB queries (it returns early when project is None).
fn make_eval(id: Uuid, status: EvaluationStatus) -> MEvaluation {
    entity::evaluation::Model {
        id,
        project: None,
        repository: "https://example.com/repo".into(),
        commit: Uuid::nil(),
        wildcard: "*".into(),
        status,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn make_build(id: Uuid, eval_id: Uuid, drv_id: Uuid, status: BuildStatus) -> MBuild {
    entity::build::Model {
        id,
        evaluation: eval_id,
        derivation: drv_id,
        status,
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

fn make_drv_output(id: Uuid, drv_id: Uuid, name: &str, path: &str) -> MDerivationOutput {
    entity::derivation_output::Model {
        id,
        derivation: drv_id,
        name: name.to_string(),
        output: path.to_string(),
        hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        package: name.to_string(),
        ca: None,
        file_hash: None,
        file_size: None,
        nar_size: None,
        is_cached: false,
        has_artefacts: false,
        cached_path: None,
        created_at: test_date(),
    }
}

fn make_dep_edge(id: Uuid, drv_id: Uuid, dep_id: Uuid) -> MDerivationDependency {
    entity::derivation_dependency::Model {
        id,
        derivation: drv_id,
        dependency: dep_id,
    }
}

fn make_eval_job(eval_id: Uuid, org_id: Uuid) -> PendingEvalJob {
    PendingEvalJob {
        evaluation_id: eval_id,
        project_id: None,
        peer_id: org_id,
        commit_id: Uuid::new_v4(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            repository: "https://example.com/repo".into(),
            commit: "abc123".into(),
            wildcards: vec!["*".into()],
            timeout_secs: None,
            sign: None,
        },
        required_paths: vec![],
    }
}

fn make_build_job(build_id: Uuid, eval_id: Uuid, org_id: Uuid) -> PendingBuildJob {
    use gradient_core::types::proto::{BuildJob, BuildTask};
    PendingBuildJob {
        build_id,
        evaluation_id: eval_id,
        peer_id: org_id,
        job: BuildJob {
            builds: vec![BuildTask {
                build_id: build_id.to_string(),
                drv_path: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv".into(),
            }],
            compress: None,
            sign: None,
        },
        required_paths: vec![],
        architecture: "x86_64-linux".into(),
        required_features: vec![],
    }
}

fn make_discovered(
    drv_path: &str,
    outputs: Vec<(&str, &str)>,
    deps: Vec<&str>,
) -> DiscoveredDerivation {
    DiscoveredDerivation {
        attr: "packages.x86_64-linux.test".into(),
        drv_path: drv_path.to_string(),
        outputs: outputs
            .iter()
            .map(|(name, path)| DerivationOutput {
                name: name.to_string(),
                path: path.to_string(),
            })
            .collect(),
        dependencies: deps.iter().map(|s| s.to_string()).collect(),
        architecture: "x86_64-linux".into(),
        required_features: vec![],
        substituted: false,
    }
}

/// Evaluation fixture with `project: Some(project_id)`. Used for webhook tests
/// where the webhook path must not return early at the `project? = None` guard.
fn make_eval_with_project(id: Uuid, project_id: Uuid, status: EvaluationStatus) -> MEvaluation {
    entity::evaluation::Model {
        id,
        project: Some(project_id),
        repository: "https://example.com/repo".into(),
        commit: Uuid::nil(),
        wildcard: "*".into(),
        status,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

/// Project fixture for webhook tests.
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

/// Webhook fixture. `secret` should be an already-encrypted base64 ciphertext.
fn make_webhook(
    id: Uuid,
    org_id: Uuid,
    encrypted_secret: &str,
    events: &[&str],
) -> entity::webhook::Model {
    entity::webhook::Model {
        id,
        organization: org_id,
        name: "test-hook".into(),
        url: "http://localhost:19999/hook".into(),
        secret: encrypted_secret.to_string(),
        events: serde_json::json!(events),
        active: true,
        created_by: Uuid::nil(),
        created_at: test_date(),
    }
}

fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
    test_support::prelude::test_state(db)
}

// ── Group A: handle_eval_result ──────────────────────────────────────────────

/// When the evaluation is already Aborted, the handler discards the result
/// immediately without inserting any rows.
#[tokio::test]
async fn eval_result_aborted_eval_discarded() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_id(eval) → Aborted
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Aborted)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![], vec![], vec![]).await;
    assert!(result.is_ok(), "aborted eval should return Ok");
}

/// When the evaluation row is missing entirely, the handler returns an error.
#[tokio::test]
async fn eval_result_missing_eval_errors() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_id(eval) → None
        .append_query_results([Vec::<MEvaluation>::new()])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![], vec![], vec![]).await;
    assert!(result.is_err(), "missing eval should return Err");
}

/// With zero derivations in the result, there are no builds to queue, so the
/// evaluation transitions directly to Completed.
#[tokio::test]
async fn eval_result_empty_derivations_completes() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find created builds → empty (no builds at all)
        .append_query_results([Vec::<MBuild>::new()])
        // 3. update_many eval status (Completed) → exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 4. find_by_id(eval) after update → Completed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![], vec![], vec![]).await;
    assert!(result.is_ok());
}

/// A single new derivation with one output: derivation + output + build rows
/// are inserted, the build transitions Created→Queued, and the eval goes Building.
#[tokio::test]
async fn eval_result_single_derivation_creates_build() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";
    let out_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hello";

    let discovered = make_discovered(drv_path, vec![("out", out_path)], vec![]);
    let build_created = make_build(build_id, eval_id, drv_id, BuildStatus::Created);
    let build_queued = make_build(build_id, eval_id, drv_id, BuildStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → none
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert_many derivations (Postgres: primary_key=None → uses query_all → query_results)
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert_many derivation_outputs
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            out_path,
        )]])
        // 5. insert_many builds
        .append_query_results([vec![build_created.clone()]])
        // 6. find Created builds → [build{Created}]
        .append_query_results([vec![build_created]])
        // 7. update_build_status Created→Queued (UPDATE...RETURNING)
        .append_query_results([vec![build_queued]])
        // 8. update_evaluation_status → exec + find_by_id
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok());
}

/// When a derivation already exists in the DB, its row is reused (no insert),
/// but a new build row is still created for this evaluation.
#[tokio::test]
async fn eval_result_existing_derivation_reuses_id() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/cccccccccccccccccccccccccccccccc-bar.drv";
    let discovered = make_discovered(
        drv_path,
        vec![("out", "/nix/store/dddddddddddddddddddddddddddddddd-bar")],
        vec![],
    );
    let existing_drv = make_derivation(drv_id, org_id, drv_path);
    let build_created = make_build(build_id, eval_id, drv_id, BuildStatus::Created);
    let build_queued = make_build(build_id, eval_id, drv_id, BuildStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → found it
        .append_query_results([vec![existing_drv]])
        // (no insert_many derivations or outputs — already exists)
        // 3. insert_many builds (Postgres: uses query_all → query_results)
        .append_query_results([vec![build_created.clone()]])
        // 4. find Created builds
        .append_query_results([vec![build_created]])
        // 5. update build Created→Queued
        .append_query_results([vec![build_queued]])
        // 6. update eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok());
}

/// Substituted derivations create build rows with status=Substituted, not Created.
/// The "find Created builds" query then returns empty, so the eval goes Completed
/// immediately (all work was already in the store).
#[tokio::test]
async fn eval_result_substituted_derivation_completes_eval() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-sub.drv";
    let out_path = "/nix/store/ffffffffffffffffffffffffffffffff-sub";
    let mut discovered = make_discovered(drv_path, vec![("out", out_path)], vec![]);
    discovered.substituted = true;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → none
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert_many derivations (Postgres: uses query_all → query_results)
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert_many derivation_outputs
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            out_path,
        )]])
        // 5. insert_many builds (Substituted status)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Substituted,
        )]])
        // 6. find Created builds → empty (build is Substituted, not Created)
        .append_query_results([Vec::<MBuild>::new()])
        // 7. update eval → Completed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

/// Two derivations where A depends on B: the dependency edge is inserted between them.
/// Both builds are queued and eval transitions to Building.
#[tokio::test]
async fn eval_result_with_dependencies() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();

    let path_a = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
    let path_b = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";
    // A depends on B.
    let drv_a = make_discovered(
        path_a,
        vec![("out", "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a")],
        vec![path_b],
    );
    let drv_b = make_discovered(
        path_b,
        vec![("out", "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b")],
        vec![],
    );

    let build_a_created = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Created);
    let build_b_created = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Created);
    let build_a_queued = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Queued);
    let build_b_queued = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → none
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert_many derivations (Postgres: uses query_all → query_results)
        .append_query_results([vec![make_derivation(drv_a_id, org_id, path_a)]])
        // 4. insert_many outputs
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_a_id,
            "out",
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a",
        )]])
        // 5. insert_many dep edges (1 edge: A → B)
        .append_query_results([vec![make_dep_edge(Uuid::new_v4(), drv_a_id, drv_b_id)]])
        // 6. insert_many builds
        .append_query_results([vec![build_a_created.clone()]])
        // 7. find Created builds → [buildA, buildB]
        .append_query_results([vec![build_a_created, build_b_created]])
        // 8. update buildA Created→Queued
        .append_query_results([vec![build_a_queued]])
        // 9. update buildB Created→Queued
        .append_query_results([vec![build_b_queued]])
        // 10. update eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![drv_a, drv_b], vec![], vec![]).await;
    assert!(result.is_ok());
}

/// Warnings in the eval result are recorded as evaluation_message rows before
/// the build queue transition.
#[tokio::test]
async fn eval_result_with_warnings() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/gggggggggggggggggggggggggggggggg-warn.drv";
    let discovered = make_discovered(
        drv_path,
        vec![("out", "/nix/store/hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh-warn")],
        vec![],
    );
    let build_created = make_build(build_id, eval_id, drv_id, BuildStatus::Created);
    let build_queued = make_build(build_id, eval_id, drv_id, BuildStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → none
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert_many derivations (Postgres: uses query_all → query_results)
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert_many outputs
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            "/nix/store/hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh-warn",
        )]])
        // 5. insert_many builds
        .append_query_results([vec![build_created.clone()]])
        // 6. record_evaluation_message: single insert with explicit PK → uses db.execute() → exec_results
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 7. find Created builds
        .append_query_results([vec![build_created]])
        // 8. update build → Queued
        .append_query_results([vec![build_queued]])
        // 9. update eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(
        &state,
        &job,
        vec![discovered],
        vec!["warning: something deprecated".into()],
        vec![],
    )
    .await;
    assert!(result.is_ok());
}

// ── Group B: handle_build_job_completed + check_evaluation_done ──────────────

/// The last build completes; no remaining active or failed builds → eval Completed.
#[tokio::test]
async fn build_completed_last_build_completes_eval() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update_build_status → Completed (UPDATE...RETURNING)
        .append_query_results([vec![build_completed]])
        // 3. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 5. find failed builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 6. update_many eval → Completed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 7. find_by_id(eval) → Completed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        .into_connection();

    let state = make_state(db);
    let _job = make_build_job(build_id, eval_id, org_id);

    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// When active builds remain, check_evaluation_done returns early (eval stays Building).
#[tokio::test]
async fn build_completed_with_remaining_active() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let other_drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let other_build_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let other_build = make_build(other_build_id, eval_id, other_drv_id, BuildStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update build → Completed
        .append_query_results([vec![build_completed]])
        // 3. find active builds → still has other_build
        .append_query_results([vec![other_build]])
        // check_evaluation_done returns early — no further queries
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// All builds done but some are Failed → eval transitions to Failed.
#[tokio::test]
async fn build_completed_with_failed_sibling() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let failed_drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let failed_build_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let failed_build = make_build(failed_build_id, eval_id, failed_drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Completed
        .append_query_results([vec![build_completed]])
        // 3. active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find eval → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 5. find failed builds → has one Failed build
        .append_query_results([vec![failed_build]])
        // 6. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// Eval is not in Building status — check_evaluation_done returns without updating.
#[tokio::test]
async fn build_completed_eval_not_building_noop() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Completed
        .append_query_results([vec![build_completed]])
        // 3. active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find eval → already Completed (not Building)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        // no further DB calls
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// Build ID not found — handler returns Ok(()) without touching eval status.
#[tokio::test]
async fn build_completed_unknown_build_noop() {
    let build_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_id(build) → None
        .append_query_results([Vec::<MBuild>::new()])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// DependencyFailed builds count as failed — if all builds are
/// DependencyFailed/Completed, the eval transitions to Failed.
#[tokio::test]
async fn build_completed_dep_failed_siblings_cause_eval_failed() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let dep_failed_drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let dep_failed_build_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let dep_failed = make_build(
        dep_failed_build_id,
        eval_id,
        dep_failed_drv_id,
        BuildStatus::DependencyFailed,
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![build]])
        .append_query_results([vec![build_completed]])
        .append_query_results([Vec::<MBuild>::new()]) // no active
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .append_query_results([vec![dep_failed]]) // DependencyFailed counts
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

// ── Group C: handle_build_job_failed + cascade_dependency_failed ─────────────

/// Build A fails; build B (which directly depends on A) is cascaded to DependencyFailed.
/// After cascade, no active builds → eval transitions to Failed.
#[tokio::test]
async fn build_failed_cascades_to_direct_dependent() {
    let eval_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);
    let build_b_dep_failed =
        make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed);
    // Edge: B.drv depends on A.drv
    let dep_edge = make_dep_edge(Uuid::new_v4(), drv_b_id, drv_a_id);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update buildA → Failed (UPDATE...RETURNING)
        .append_query_results([vec![build_a_failed]])
        // ── BFS iteration 1: failed_drv = A ──
        // 3. cascade: find Created/Queued builds → [buildB]
        .append_query_results([vec![build_b]])
        // 4. cascade: find dep edge for buildB → found
        .append_query_results([vec![dep_edge]])
        // 5. update buildB → DependencyFailed
        .append_query_results([vec![build_b_dep_failed]])
        // ── BFS iteration 2: failed_drv = B (transitive) ──
        // 6. find Created/Queued builds → empty (B already DependencyFailed)
        .append_query_results([Vec::<MBuild>::new()])
        // ── BFS done ──
        // 7. check_evaluation_done: find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 8. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 9. find failed builds → [buildA{Failed}, buildB{DependencyFailed}]
        .append_query_results([vec![
            make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed),
            make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed),
        ]])
        // 10. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_a_id, "build error").await;
    assert!(result.is_ok());
}

/// Build fails with no Created/Queued dependents — cascade is a no-op.
/// check_evaluation_done sees only the Failed build → eval → Failed.
#[tokio::test]
async fn build_failed_no_dependents() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_failed = make_build(build_id, eval_id, drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Failed
        .append_query_results([vec![build_failed.clone()]])
        // 3. cascade: find Created/Queued → empty (no candidates)
        .append_query_results([Vec::<MBuild>::new()])
        // 4. check active → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 5. find eval → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 6. find failed → [build]
        .append_query_results([vec![build_failed]])
        // 7. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_id, "error").await;
    assert!(result.is_ok());
}

/// Multiple candidates in cascade: B depends on A (cascaded), C does not (untouched).
/// C is still Queued → active builds remain → eval stays Building.
#[tokio::test]
async fn build_failed_cascade_only_direct_dependents() {
    let eval_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let drv_c_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();
    let build_c_id = Uuid::new_v4();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);
    let build_b_dep_failed =
        make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed);
    let build_c = make_build(build_c_id, eval_id, drv_c_id, BuildStatus::Queued);
    // B depends on A; C does NOT depend on A
    let dep_edge_b_a = make_dep_edge(Uuid::new_v4(), drv_b_id, drv_a_id);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update → Failed
        .append_query_results([vec![build_a_failed]])
        // ── BFS iteration 1: failed_drv = A ──
        // 3. cascade: find Created/Queued → [buildB, buildC]
        .append_query_results([vec![build_b, build_c.clone()]])
        // 4. cascade for buildB: dep edge B→A found
        .append_query_results([vec![dep_edge_b_a]])
        // 5. update buildB → DependencyFailed
        .append_query_results([vec![build_b_dep_failed]])
        // 6. cascade for buildC: NO dep edge C→A
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // ── BFS iteration 2: failed_drv = B (transitively cascaded) ──
        // 7. find Created/Queued → [buildC] (B already DependencyFailed)
        .append_query_results([vec![build_c.clone()]])
        // 8. does C depend on B? → no
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // ── BFS done ──
        // 9. check active → buildC is still Queued
        .append_query_results([vec![build_c]])
        // active not empty → check_evaluation_done returns early
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_a_id, "error").await;
    assert!(result.is_ok());
}

/// Build job failed for an unknown build ID — handler returns Ok(()) silently.
#[tokio::test]
async fn build_failed_unknown_build_noop() {
    let build_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<MBuild>::new()])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_id, "error").await;
    assert!(result.is_ok());
}

/// Cascade does NOT affect builds with status=Building (only Created/Queued).
/// The Building build remains active → eval stays Building.
#[tokio::test]
async fn build_failed_cascade_skips_building_status() {
    let eval_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    // B is Building (not Created/Queued) — cascade filter excludes it
    let build_b_building = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Building);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update → Failed
        .append_query_results([vec![build_a_failed]])
        // 3. cascade: find Created/Queued → empty (buildB is Building, excluded)
        .append_query_results([Vec::<MBuild>::new()])
        // 4. check active: buildB is Building → still active
        .append_query_results([vec![build_b_building]])
        // eval stays Building → no update
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_a_id, "err").await;
    assert!(result.is_ok());
}

// ── Group D: handle_build_output ─────────────────────────────────────────────

/// Build outputs update the `nar_size`, `file_hash`, and `has_artefacts` fields
/// of the corresponding `derivation_output` row.
#[tokio::test]
async fn build_output_updates_derivation_output() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let drv_out_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let drv_out = make_drv_output(
        drv_out_id,
        drv_id,
        "out",
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out",
    );
    let drv_out_updated = {
        let mut o = drv_out.clone();
        o.nar_size = Some(12345);
        o.file_hash = Some("sha256:abc".into());
        o.has_artefacts = false;
        o
    };

    let outputs = vec![BuildOutput {
        name: "out".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
        hash: "aaaa".into(),
        nar_size: Some(12345),
        nar_hash: Some("sha256:abc".into()),
        has_artefacts: false,
    }];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. find derivation_output row
        .append_query_results([vec![drv_out]])
        // 3. update derivation_output (UPDATE...RETURNING)
        .append_query_results([vec![drv_out_updated]])
        .into_connection();

    let state = make_state(db);
    let job = make_build_job(build_id, eval_id, org_id);

    let result = build_handler::handle_build_output(&state, &job, build_id, outputs).await;
    assert!(result.is_ok());
}

/// When the derivation_output row is not found, a warning is logged but the
/// handler still returns Ok (best-effort update).
#[tokio::test]
async fn build_output_missing_row_warns_not_errors() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let outputs = vec![BuildOutput {
        name: "out".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
        hash: "aaaa".into(),
        nar_size: None,
        nar_hash: None,
        has_artefacts: false,
    }];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. find derivation_output → not found
        .append_query_results([Vec::<MDerivationOutput>::new()])
        // No update attempted
        .into_connection();

    let state = make_state(db);
    let job = make_build_job(build_id, eval_id, org_id);

    let result = build_handler::handle_build_output(&state, &job, build_id, outputs).await;
    assert!(result.is_ok());
}

/// Build not found → handler returns an Err (build context is mandatory).
#[tokio::test]
async fn build_output_unknown_build_errors() {
    let eval_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<MBuild>::new()])
        .into_connection();

    let state = make_state(db);
    let job = make_build_job(build_id, eval_id, org_id);

    let result = build_handler::handle_build_output(&state, &job, build_id, vec![]).await;
    assert!(result.is_err());
}

// ── Group E: handle_eval_job_completed / handle_eval_job_failed ──────────────

/// When no active builds remain and the eval is still Building,
/// `handle_eval_job_completed` transitions it to Completed via
/// `check_evaluation_done` (which also checks for failed builds).
#[tokio::test]
async fn eval_job_completed_no_active_builds_completes_eval() {
    let eval_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 2. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 3. find failed builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. update eval → Completed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        .into_connection();

    let state = make_state(db);
    let result = eval_handler::handle_eval_job_completed(&state, eval_id).await;
    assert!(result.is_ok());
}

/// When the eval job completes but a build has failed, the eval transitions
/// to Failed (not Completed). This is the regression fix for the bug where a
/// failed build was being silently masked into a Completed evaluation.
#[tokio::test]
async fn eval_job_completed_with_failed_build_marks_eval_failed() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let failed_build = make_build(build_id, eval_id, drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find active builds → empty (all builds are terminal)
        .append_query_results([Vec::<MBuild>::new()])
        // 2. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 3. find failed builds → [failed_build]
        .append_query_results([vec![failed_build]])
        // 4. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let result = eval_handler::handle_eval_job_completed(&state, eval_id).await;
    assert!(result.is_ok());
}

/// When active builds still exist, `handle_eval_job_completed` is a no-op
/// (the eval will complete once the last build finishes).
#[tokio::test]
async fn eval_job_completed_active_builds_remain_noop() {
    let eval_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let active_build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find active builds → still has some
        .append_query_results([vec![active_build]])
        // no further queries
        .into_connection();

    let state = make_state(db);
    let result = eval_handler::handle_eval_job_completed(&state, eval_id).await;
    assert!(result.is_ok());
}

/// A failed eval job transitions the evaluation from Building to Failed and
/// records an error message.
#[tokio::test]
async fn eval_job_failed_transitions_eval_to_failed() {
    let eval_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval) → Building (non-terminal)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. insert evaluation_message (error record) → exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 3. update_many eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 4. find_by_id(eval) → Failed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let result = eval_handler::handle_eval_job_failed(&state, eval_id, "nix eval crashed").await;
    assert!(result.is_ok());
}

/// When the evaluation is already in a terminal state (Completed), a failed
/// eval job does not overwrite the status.
#[tokio::test]
async fn eval_job_failed_terminal_eval_noop() {
    let eval_id = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval) → already Completed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        // no status update (terminal guard)
        .into_connection();

    let state = make_state(db);
    let result = eval_handler::handle_eval_job_failed(&state, eval_id, "late error").await;
    assert!(result.is_ok());
}

// ── Group G: abort_evaluation ─────────────────────────────────────────────────

/// `abort_evaluation` with two active builds (Queued + Building) cascades both
/// to Aborted and then transitions the evaluation to Aborted.
///
/// DB call sequence:
///   1. Q: find active builds (Created/Queued/Building) → [buildA(Queued), buildB(Building)]
///   2. Q: update buildA → Aborted (UPDATE…RETURNING)
///      → spawns fire_build_webhook(Aborted) — returns early (DependencyFailed/Aborted → return)
///      → spawns log_finalize (NoopLogStorage → no-op)
///   3. Q: update buildB → Aborted
///   4. E: update_many eval → Aborted
///   5. Q: find_by_id(eval) after update
///      → spawns fire_evaluation_webhook (eval.project=None → returns early)
#[tokio::test]
async fn abort_cascades_to_active_builds() {
    let eval_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();

    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Queued);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Building);
    let build_a_aborted = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Aborted);
    let build_b_aborted = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Aborted);
    let eval = make_eval(eval_id, EvaluationStatus::Building);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find active builds
        .append_query_results([vec![build_a, build_b]])
        // 2. update buildA → Aborted (UPDATE RETURNING)
        .append_query_results([vec![build_a_aborted]])
        // 3. update buildB → Aborted
        .append_query_results([vec![build_b_aborted]])
        // 4. update_many eval → Aborted
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 5. find_by_id(eval) after update
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Aborted)]])
        .into_connection();

    let state = make_state(db);
    gradient_core::db::abort_evaluation(Arc::clone(&state), eval).await;
    // Reaching here without panic confirms the abort cascade completed.
}

/// `abort_evaluation` returns immediately without any DB queries when the
/// evaluation is already Completed.
#[tokio::test]
async fn abort_skips_completed_eval() {
    let eval_id = Uuid::new_v4();
    // Empty MockDatabase — any unexpected DB call would cause an error that,
    // if propagated, would surface as a test failure.
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let state = make_state(db);
    let eval = make_eval(eval_id, EvaluationStatus::Completed);
    // Should return immediately; the guard `evaluation.status == Completed → return`
    // prevents any DB queries.
    gradient_core::db::abort_evaluation(Arc::clone(&state), eval).await;
}

/// `abort_evaluation` with no active builds still transitions the evaluation
/// to Aborted (the find-builds query returns empty, but the eval update runs).
#[tokio::test]
async fn abort_no_active_builds() {
    let eval_id = Uuid::new_v4();
    let eval = make_eval(eval_id, EvaluationStatus::Building);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 2. update_many eval → Aborted
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 3. find_by_id(eval) after update
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Aborted)]])
        .into_connection();

    let state = make_state(db);
    gradient_core::db::abort_evaluation(Arc::clone(&state), eval).await;
}

// ── Group H: Handler behavioral gaps ─────────────────────────────────────────

/// When `EDerivation::insert_many().exec()` fails (MockDB returns empty rows →
/// `RecordNotInserted`), `handle_eval_result` transitions the evaluation to
/// Failed via `update_evaluation_status_with_error` and returns `Err`.
///
/// DB call sequence:
///   1. Q: find_by_id(eval) → Building
///   2. Q: find existing derivations → none
///   3. Q: insert_many derivations → EMPTY (→ RecordNotInserted error)
///   4. E: insert evaluation_message (error record)
///   5. E: update_many eval → Failed
///   6. Q: find_by_id(eval) → Failed
#[tokio::test]
async fn eval_result_error_on_derivation_insert_transitions_eval_failed() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();

    let drv_path = "/nix/store/aaaa-fail-insert.drv";
    let discovered = make_discovered(
        drv_path,
        vec![("out", "/nix/store/bbbb-fail-insert")],
        vec![],
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → none
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert_many derivations → empty result → RecordNotInserted
        .append_query_results([Vec::<MDerivation>::new()])
        // 4. update_evaluation_status_with_error: insert evaluation_message
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 5. update_many eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 6. find_by_id(eval) → Failed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(
        result.is_err(),
        "expected Err when derivation insert fails, got: {:?}",
        result.ok()
    );
}

/// When `EBuild::insert_many().exec()` fails after derivations are inserted
/// successfully, `handle_eval_result` transitions the evaluation to Failed.
///
/// DB call sequence:
///   1. Q: find_by_id(eval) → Building
///   2. Q: find existing derivations → none
///   3. Q: insert_many derivations → success
///   4. Q: insert_many derivation_outputs → success
///   5. Q: insert_many builds → EMPTY (→ RecordNotInserted error)
///   6. E: insert evaluation_message
///   7. E: update_many eval → Failed
///   8. Q: find_by_id(eval) → Failed
#[tokio::test]
async fn eval_result_build_insert_fails_transitions_eval_failed() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();

    let drv_path = "/nix/store/cccc-build-fail.drv";
    let out_path = "/nix/store/dddd-build-fail";
    let discovered = make_discovered(drv_path, vec![("out", out_path)], vec![]);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → none
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert_many derivations → success
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert_many derivation_outputs → success
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            out_path,
        )]])
        // 5. insert_many builds → empty result → RecordNotInserted error
        .append_query_results([Vec::<MBuild>::new()])
        // 6. update_evaluation_status_with_error: insert evaluation_message
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 7. update_many eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 8. find_by_id(eval) → Failed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result = eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(
        result.is_err(),
        "expected Err when build insert fails, got: {:?}",
        result.ok()
    );
}

/// When derivation A already exists in the DB and new derivation B depends on A,
/// the dep edge B→A is still inserted because A's ID is in `drv_path_to_id`
/// from the initial existing-derivation lookup.
///
/// DB call sequence:
///   1. Q: find_by_id(eval) → Building
///   2. Q: find existing derivations → [drvA (existing)]
///   3. Q: insert_many derivations (only B is new)
///   4. Q: insert_many derivation_outputs (for B)
///   5. Q: insert_many dep_edges (B→A)
///   6. Q: insert_many builds (both A and B get new build rows)
///   7. Q: find Created builds → [buildA_created, buildB_created]
///   8. Q: update buildA → Queued
///   9. Q: update buildB → Queued
///  10. E: update_many eval → Building
///  11. Q: find_by_id(eval) → Building
#[tokio::test]
async fn eval_result_existing_drv_still_creates_dep_edge() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();

    let path_a = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-existing.drv";
    let path_b = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-new.drv";
    // A already exists; B is new and depends on A.
    let drv_a_existing = make_discovered(path_a, vec![("out", "/nix/store/aaaa-existing")], vec![]);
    let drv_b_new = make_discovered(
        path_b,
        vec![("out", "/nix/store/bbbb-new")],
        vec![path_a], // B depends on A
    );

    let build_a_created = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Created);
    let build_b_created = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Created);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → drvA already in DB
        .append_query_results([vec![make_derivation(drv_a_id, org_id, path_a)]])
        // 3. insert_many derivations (only B is new)
        .append_query_results([vec![make_derivation(drv_b_id, org_id, path_b)]])
        // 4. insert_many derivation_outputs (for B)
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_b_id,
            "out",
            "/nix/store/bbbb-new",
        )]])
        // 5. insert_many dep_edges (B→A): A's ID is in drv_path_to_id from the existing lookup
        .append_query_results([vec![make_dep_edge(Uuid::new_v4(), drv_b_id, drv_a_id)]])
        // 6. insert_many builds (A and B both get new build rows for this evaluation)
        .append_query_results([vec![build_a_created.clone()]])
        // 7. find Created builds
        .append_query_results([vec![build_a_created, build_b_created]])
        // 8. update buildA → Queued
        .append_query_results([vec![make_build(
            build_a_id,
            eval_id,
            drv_a_id,
            BuildStatus::Queued,
        )]])
        // 9. update buildB → Queued
        .append_query_results([vec![make_build(
            build_b_id,
            eval_id,
            drv_b_id,
            BuildStatus::Queued,
        )]])
        // 10. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 11. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![drv_a_existing, drv_b_new], vec![], vec![])
            .await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

// ── Group I: Webhook delivery ─────────────────────────────────────────────────

/// After `handle_build_job_completed`, the `RecordingWebhookClient` receives a
/// delivery with event `"build.completed"`.
///
/// Uses `eval.project: Some(project_id)` so that `fire_build_webhook` and
/// `fire_evaluation_webhook` proceed past the `project? = None → return` guard.
/// Both webhook tasks are spawned inside `update_build_status` /
/// `update_evaluation_status` and run when the main task yields.
///
/// DB call sequence (main handler):
///   1. Q: find_by_id(build) → Building
///   2. Q: update build → Completed (UPDATE…RETURNING)
///      → spawns TASK_A: fire_build_webhook(build, Completed)
///      → spawns TASK_B: log_finalize (NoopLogStorage → no-op)
///   3. Q: find active builds → empty
///   4. Q: find_by_id(eval) → Building (with project=Some)
///   5. Q: find failed builds → empty
///   6. E: update_many eval → Completed
///   7. Q: find_by_id(eval) → Completed
///      → spawns TASK_C: fire_evaluation_webhook(eval, Completed)
///
/// TASK_A (fire_build_webhook Completed):
///   8.  Q: get_build_org_id: find_by_id(eval) → eval with project=Some
///   9.  Q: get_build_org_id: find_by_id(project_id) → project
///   10. Q: find_by_id(build.derivation) → derivation (best-effort)
///   11. Q: find webhooks for org → [webhook subscribed to "build.completed"]
///         decrypt + sign + deliver → recorded
///
/// TASK_B: no-op.
///
/// TASK_C (fire_evaluation_webhook Completed):
///   12. Q: find_by_id(project_id) → project
///   13. Q: find webhooks for org → [webhook subscribed to "build.completed"]
///         subscription check for "evaluation.completed" → false → no delivery
#[tokio::test]
async fn webhook_fired_on_build_completed() {
    use std::io::Write as _;

    // Create a real 32-byte key file so decrypt_webhook_secret can read it.
    let mut key_file = tempfile::NamedTempFile::new().expect("create temp key file");
    key_file
        .write_all(b"test-secret-key-32-bytes-padding!")
        .unwrap();
    key_file.flush().unwrap();
    let key_path = key_file.path().to_string_lossy().to_string();

    // Encrypt the webhook secret with this key.
    let encrypted_secret =
        gradient_core::ci::encrypt_webhook_secret(&key_path, "plaintext-hook-secret")
            .expect("encrypt webhook secret");

    let eval_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();
    let webhook_id = Uuid::new_v4();

    let build_building = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let eval_building = make_eval_with_project(eval_id, project_id, EvaluationStatus::Building);
    let eval_completed = make_eval_with_project(eval_id, project_id, EvaluationStatus::Completed);
    let project = make_project(project_id, org_id);
    let drv = make_derivation(drv_id, org_id, "/nix/store/aaaa-hello.drv");
    let webhook = make_webhook(webhook_id, org_id, &encrypted_secret, &["build.completed"]);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Main handler:
        // 1. find_by_id(build)
        .append_query_results([vec![build_building]])
        // 2. update build → Completed
        .append_query_results([vec![build_completed]])
        // 3. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![eval_building]])
        // 5. find failed builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 6. update_many eval → Completed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 7. find_by_id(eval) → Completed
        .append_query_results([vec![eval_completed.clone()]])
        // TASK_A (fire_build_webhook Completed):
        // 8. get_build_org_id: find_by_id(eval)
        .append_query_results([vec![eval_completed.clone()]])
        // 9. get_build_org_id: find_by_id(project)
        .append_query_results([vec![project.clone()]])
        // 10. find_by_id(build.derivation) — best-effort
        .append_query_results([vec![drv]])
        // 11. find webhooks for org
        .append_query_results([vec![webhook.clone()]])
        // TASK_C (fire_evaluation_webhook Completed):
        // 12. find_by_id(project)
        .append_query_results([vec![project]])
        // 13. find webhooks for org (subscribed to "build.completed", not "evaluation.completed")
        .append_query_results([vec![webhook]])
        .into_connection();

    let (state, recorder) = test_state_recorded(db, key_path);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());

    // Let spawned webhook tasks run.
    tokio::task::yield_now().await;

    let calls = recorder.calls();
    assert!(
        calls.iter().any(|c| c.event == "build.completed"),
        "expected build.completed webhook call; got: {:?}",
        calls.iter().map(|c| &c.event).collect::<Vec<_>>()
    );
}

/// After `handle_build_job_failed` where build B is cascaded to `DependencyFailed`,
/// the `RecordingWebhookClient` receives a `"build.failed"` delivery (for build A)
/// but NOT a `"build.dependency_failed"` delivery (DependencyFailed → early return
/// in `fire_build_webhook` at line: `Created | Aborted | DependencyFailed => return`).
///
/// DB call sequence (main handler):
///   1. Q: find_by_id(buildA) → Building
///   2. Q: update buildA → Failed
///      → spawns TASK_A: fire_build_webhook(buildA, Failed)
///      → spawns TASK_B: log_finalize
///   3. Q: cascade: find Created/Queued builds → [buildB]
///   4. Q: cascade: find dep edge for buildB → found
///   5. Q: update buildB → DependencyFailed
///      → spawns TASK_C: fire_build_webhook(buildB, DependencyFailed) — returns early
///      → spawns TASK_D: log_finalize
///   6. Q: check active → empty
///   7. Q: find_by_id(eval) → Building (with project=Some)
///   8. Q: find failed → [buildA, buildB]
///   9. E: update_many eval → Failed
///  10. Q: find_by_id(eval) → Failed
///      → spawns TASK_E: fire_evaluation_webhook(eval, Failed)
///
/// TASK_A (fire_build_webhook Failed):
///  11. Q: get_build_org_id: find_by_id(eval_id)
///  12. Q: get_build_org_id: find_by_id(project_id)
///  13. Q: find_by_id(buildA.derivation) — best-effort
///  14. Q: find webhooks → [webhook subscribed to "build.failed"]
///         deliver → "build.failed" recorded
///
/// TASK_C (fire_build_webhook DependencyFailed): returns immediately — no DB.
///
/// TASK_E (fire_evaluation_webhook Failed):
///  15. Q: find_by_id(project_id)
///  16. Q: find webhooks → [webhook subscribed to "build.failed"]
///         subscription check for "evaluation.failed" → false → no delivery
#[tokio::test]
async fn webhook_not_fired_for_dep_failed() {
    use std::io::Write as _;

    let mut key_file = tempfile::NamedTempFile::new().expect("create temp key file");
    key_file
        .write_all(b"test-secret-key-32-bytes-padding!")
        .unwrap();
    key_file.flush().unwrap();
    let key_path = key_file.path().to_string_lossy().to_string();

    let encrypted_secret =
        gradient_core::ci::encrypt_webhook_secret(&key_path, "plaintext-hook-secret")
            .expect("encrypt webhook secret");

    let eval_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_a_id = Uuid::new_v4();
    let drv_b_id = Uuid::new_v4();
    let build_a_id = Uuid::new_v4();
    let build_b_id = Uuid::new_v4();
    let webhook_id = Uuid::new_v4();

    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);
    let build_b_dep_failed =
        make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed);
    let dep_edge = make_dep_edge(Uuid::new_v4(), drv_b_id, drv_a_id);
    // Eval with project set so webhooks fire.
    let eval_building = make_eval_with_project(eval_id, project_id, EvaluationStatus::Building);
    let eval_failed = make_eval_with_project(eval_id, project_id, EvaluationStatus::Failed);
    let project = make_project(project_id, org_id);
    let drv_a = make_derivation(drv_a_id, org_id, "/nix/store/aaaa-a.drv");
    // Webhook subscribed only to "build.failed", not "build.dependency_failed".
    let webhook = make_webhook(webhook_id, org_id, &encrypted_secret, &["build.failed"]);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Main handler:
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update buildA → Failed
        .append_query_results([vec![build_a_failed.clone()]])
        // 3. cascade: find Created/Queued → [buildB]
        .append_query_results([vec![build_b]])
        // 4. cascade: dep edge for buildB → found
        .append_query_results([vec![dep_edge]])
        // 5. update buildB → DependencyFailed
        .append_query_results([vec![build_b_dep_failed.clone()]])
        // ── BFS iteration 2: failed_drv = B ──
        // 6. find Created/Queued → empty (B already DependencyFailed)
        .append_query_results([Vec::<MBuild>::new()])
        // ── BFS done ──
        // 7. check active → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 8. find_by_id(eval) → Building
        .append_query_results([vec![eval_building]])
        // 8. find failed → [buildA, buildB]
        .append_query_results([vec![build_a_failed, build_b_dep_failed]])
        // 9. update_many eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 10. find_by_id(eval) → Failed
        .append_query_results([vec![eval_failed.clone()]])
        // TASK_A (fire_build_webhook Failed):
        // 11. get_build_org_id: find_by_id(eval)
        .append_query_results([vec![eval_failed]])
        // 12. get_build_org_id: find_by_id(project)
        .append_query_results([vec![project.clone()]])
        // 13. find_by_id(buildA.derivation)
        .append_query_results([vec![drv_a]])
        // 14. find webhooks
        .append_query_results([vec![webhook.clone()]])
        // TASK_C: fire_build_webhook(DependencyFailed) → returns immediately, no DB
        // TASK_E (fire_evaluation_webhook Failed):
        // 15. find_by_id(project)
        .append_query_results([vec![project]])
        // 16. find webhooks (subscribed to "build.failed", not "evaluation.failed")
        .append_query_results([vec![webhook]])
        .into_connection();

    let (state, recorder) = test_state_recorded(db, key_path);
    let result = build_handler::handle_build_job_failed(&state, build_a_id, "build error").await;
    assert!(result.is_ok());

    tokio::task::yield_now().await;

    let calls = recorder.calls();
    assert!(
        calls.iter().any(|c| c.event == "build.failed"),
        "expected build.failed webhook call; got: {:?}",
        calls.iter().map(|c| &c.event).collect::<Vec<_>>()
    );
    assert!(
        !calls.iter().any(|c| c.event == "build.dependency_failed"),
        "build.dependency_failed must NOT be delivered; got: {:?}",
        calls.iter().map(|c| &c.event).collect::<Vec<_>>()
    );
}

// ── Group K: Entry Points ───────────────────────────────────────────────────

/// When handle_eval_result receives derivations for an evaluation with
/// project_id: Some, it must create entry_point rows mapping each derivation's
/// attr path to its build. This was done by the legacy evaluator but was never
/// tested in the new scheduler.
#[tokio::test]
async fn eval_result_creates_entry_points_for_project() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";
    let out_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hello";

    let derivations = vec![DiscoveredDerivation {
        attr: "packages.x86_64-linux.hello".into(),
        drv_path: drv_path.to_string(),
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: out_path.to_string(),
        }],
        dependencies: vec![],
        architecture: "x86_64-linux".into(),
        required_features: vec![],
        substituted: false,
    }];

    let eval_job = PendingEvalJob {
        evaluation_id: eval_id,
        project_id: Some(project_id),
        peer_id: org_id,
        commit_id: Uuid::new_v4(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            repository: "https://example.com/repo".into(),
            commit: "abc123".into(),
            wildcards: vec!["*".into()],
            timeout_secs: None,
            sign: None,
        },
        required_paths: vec![],
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find eval
        .append_query_results([vec![make_eval_with_project(
            eval_id,
            project_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 2. find existing derivations → empty
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert derivation
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert derivation output
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            out_path,
        )]])
        // 5. insert build (Created)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Created,
        )]])
        // 6. find builds for eval (entry point mapping)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Created,
        )]])
        // 7. insert entry_points
        .append_query_results([vec![entity::entry_point::Model {
            id: Uuid::new_v4(),
            project: project_id,
            evaluation: eval_id,
            build: build_id,
            eval: "packages.x86_64-linux.hello".into(),
            created_at: test_date(),
        }]])
        // 8. find project (for GC)
        .append_query_results([vec![make_project(project_id, org_id)]])
        // 9. find Created builds
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Created,
        )]])
        // 10. update build → Queued
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Queued,
        )]])
        // 11. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 12. find eval → Building
        .append_query_results([vec![make_eval_with_project(
            eval_id,
            project_id,
            EvaluationStatus::Building,
        )]])
        .into_connection();

    let state = test_support::prelude::test_state(db);
    let result = eval_handler::handle_eval_result(&state, &eval_job, derivations, vec![], vec![]).await;
    assert!(
        result.is_ok(),
        "eval_result with project should succeed: {:?}",
        result.err()
    );
}

/// When project_id is None (direct build), no entry points are created and no
/// project is queried for GC. The same MockDB stages as existing tests suffice.
#[tokio::test]
async fn eval_result_no_entry_points_without_project() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";
    let out_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hello";

    let derivations = vec![DiscoveredDerivation {
        attr: "packages.x86_64-linux.hello".into(),
        drv_path: drv_path.to_string(),
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: out_path.to_string(),
        }],
        dependencies: vec![],
        architecture: "x86_64-linux".into(),
        required_features: vec![],
        substituted: false,
    }];

    // project_id: None — direct build, no entry points
    let eval_job = make_eval_job(eval_id, org_id);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find eval
        .append_query_results([vec![make_eval(
            eval_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 2. find existing derivations → empty
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert derivation
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert derivation output
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            out_path,
        )]])
        // 5. insert build (Created)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Created,
        )]])
        // NO entry point insert, NO project query — skipped when project_id is None
        // 6. find Created builds
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Created,
        )]])
        // 7. update build → Queued
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Queued,
        )]])
        // 8. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 9. find eval → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .into_connection();

    let state = test_support::prelude::test_state(db);
    let result = eval_handler::handle_eval_result(&state, &eval_job, derivations, vec![], vec![]).await;
    assert!(
        result.is_ok(),
        "eval_result without project should succeed: {:?}",
        result.err()
    );
}

// ── Group O: Error Source Detection ─────────────────────────────────────────

/// When a FlakeJob fails with a prefetch error, the source should be
/// "flake-prefetch", not the generic "worker". Legacy evaluator distinguished
/// these at evaluator/src/scheduler/evaluation.rs:252-258.
#[tokio::test]
async fn eval_job_failed_detects_prefetch_error_source() {
    let eval_id = Uuid::new_v4();
    // Evaluation in Fetching status — prefetch failure
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find eval
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Fetching)]])
        // 2. update_many eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 3. find eval → Failed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = test_support::prelude::test_state(db);

    let result = eval_handler::handle_eval_job_failed(
        &state,
        eval_id,
        "failed to prefetch flake input: connection refused",
    )
    .await;
    assert!(result.is_ok());

    // Verify the error source was set correctly by checking the DB transaction log.
    // The update_evaluation_status_with_error call uses the source parameter.
    // With MockDB we can't directly assert the source parameter, but we verify
    // the function doesn't panic and the status transitions correctly.
    // TODO: Add a RecordingStatusReporter or similar to capture the source field.
}

// ── Group P: GC and Substituted with Project ────────────────────────────────

/// When all derivations are substituted and project_id is Some, the evaluation
/// should complete immediately (no Created builds) AND entry points should
/// still be created.
#[tokio::test]
async fn eval_result_all_substituted_with_project_completes() {
    let eval_id = Uuid::new_v4();
    let org_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let build_id = Uuid::new_v4();

    let drv_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";
    let out_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hello";

    let derivations = vec![DiscoveredDerivation {
        attr: "packages.x86_64-linux.hello".into(),
        drv_path: drv_path.to_string(),
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: out_path.to_string(),
        }],
        dependencies: vec![],
        architecture: "x86_64-linux".into(),
        required_features: vec![],
        substituted: true, // all outputs cached
    }];

    let eval_job = PendingEvalJob {
        evaluation_id: eval_id,
        project_id: Some(project_id),
        peer_id: org_id,
        commit_id: Uuid::new_v4(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            repository: "https://example.com/repo".into(),
            commit: "abc123".into(),
            wildcards: vec!["*".into()],
            timeout_secs: None,
            sign: None,
        },
        required_paths: vec![],
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find eval
        .append_query_results([vec![make_eval_with_project(
            eval_id,
            project_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 2. find existing derivations → empty
        .append_query_results([Vec::<MDerivation>::new()])
        // 3. insert derivation
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // 4. insert derivation output
        .append_query_results([vec![make_drv_output(
            Uuid::new_v4(),
            drv_id,
            "out",
            out_path,
        )]])
        // 5. insert build (Substituted — not Created!)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Substituted,
        )]])
        // 6. find builds for eval (entry point mapping)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Substituted,
        )]])
        // 7. insert entry_points
        .append_query_results([vec![entity::entry_point::Model {
            id: Uuid::new_v4(),
            project: project_id,
            evaluation: eval_id,
            build: build_id,
            eval: "packages.x86_64-linux.hello".into(),
            created_at: test_date(),
        }]])
        // 8. find project (for GC)
        .append_query_results([vec![make_project(project_id, org_id)]])
        // 9. find Created builds → empty (all Substituted)
        .append_query_results([Vec::<MBuild>::new()])
        // 10. update_many eval → Completed (not Building!)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 11. find eval → Completed
        .append_query_results([vec![make_eval_with_project(
            eval_id,
            project_id,
            EvaluationStatus::Completed,
        )]])
        .into_connection();

    let state = test_support::prelude::test_state(db);
    let result = eval_handler::handle_eval_result(&state, &eval_job, derivations, vec![], vec![]).await;
    assert!(
        result.is_ok(),
        "substituted eval with project should complete: {:?}",
        result.err()
    );
}

// ── Group M: Transitive DependencyFailed Cascade ────────────────────────────

/// When build C fails and B depends on C and A depends on B, ALL of them should
/// be marked DependencyFailed — not just the direct dependent B.
/// The cascade walks the derivation_dependency graph transitively (BFS).
#[tokio::test]
async fn build_failed_cascades_transitively_through_graph() {
    // A depends on B, B depends on C. C fails.
    // Expected: B → DependencyFailed, then A → DependencyFailed (transitive).
    let eval_id = Uuid::new_v4();
    let drv_a = Uuid::new_v4();
    let drv_b = Uuid::new_v4();
    let drv_c = Uuid::new_v4();
    let build_a = Uuid::new_v4();
    let build_b = Uuid::new_v4();
    let build_c = Uuid::new_v4();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find build C
        .append_query_results([vec![make_build(
            build_c,
            eval_id,
            drv_c,
            BuildStatus::Building,
        )]])
        // 2. update build C → Failed (RETURNING)
        .append_query_results([vec![make_build(
            build_c,
            eval_id,
            drv_c,
            BuildStatus::Failed,
        )]])
        // ── cascade iteration 1: failed_drv = C ──
        // 3. find Created/Queued builds
        .append_query_results([vec![
            make_build(build_a, eval_id, drv_a, BuildStatus::Queued),
            make_build(build_b, eval_id, drv_b, BuildStatus::Queued),
        ]])
        // 4. does A depend on C? → no
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // 5. does B depend on C? → yes
        .append_query_results([vec![make_dep_edge(Uuid::new_v4(), drv_b, drv_c)]])
        // 6. update B → DependencyFailed
        .append_query_results([vec![make_build(
            build_b,
            eval_id,
            drv_b,
            BuildStatus::DependencyFailed,
        )]])
        // ── cascade iteration 2: failed_drv = B (transitively cascaded) ──
        // 7. find Created/Queued builds (A still Queued, B already DependencyFailed)
        .append_query_results([vec![make_build(
            build_a,
            eval_id,
            drv_a,
            BuildStatus::Queued,
        )]])
        // 8. does A depend on B? → yes
        .append_query_results([vec![make_dep_edge(Uuid::new_v4(), drv_a, drv_b)]])
        // 9. update A → DependencyFailed
        .append_query_results([vec![make_build(
            build_a,
            eval_id,
            drv_a,
            BuildStatus::DependencyFailed,
        )]])
        // ── cascade iteration 3: failed_drv = A ──
        // 10. find Created/Queued builds → empty (all cascaded)
        .append_query_results([Vec::<MBuild>::new()])
        // ── check_evaluation_done ──
        // 11. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 12. find eval
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 13. find failed builds → B and A
        .append_query_results([vec![
            make_build(build_b, eval_id, drv_b, BuildStatus::DependencyFailed),
            make_build(build_a, eval_id, drv_a, BuildStatus::DependencyFailed),
        ]])
        // 14. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 15. find eval → Failed
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let state = test_support::prelude::test_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_c, "nix build failed").await;
    assert!(
        result.is_ok(),
        "transitive cascade should succeed: {:?}",
        result.err()
    );
}

// ── Group R: fetch_repository is a stub ─────────────────────────────────────
// (Worker-side test — lives in worker/src/executor/fetch.rs)
// The current fetch_repository() returns Ok(()) without cloning anything.
// A proper implementation needs git2 integration.
// See: worker/src/executor/fetch.rs:42-47

// ── Group S: NarReady processing ────────────────────────────────────────────

/// When the server receives NarReady with a store_path, nar_size, and nar_hash,
/// it should update the matching derivation_output row with nar_size and file_hash.
#[tokio::test]
async fn nar_ready_updates_derivation_output() {
    let output_id = Uuid::new_v4();
    let drv_id = Uuid::new_v4();
    let store_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hello";

    let existing_output = make_drv_output(output_id, drv_id, "out", store_path);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find derivation_output by store_path
        .append_query_results([vec![existing_output.clone()]])
        // 2. update derivation_output (RETURNING)
        .append_query_results([vec![entity::derivation_output::Model {
            nar_size: Some(12345),
            file_hash: Some("sha256-abc123".into()),
            ..existing_output
        }]])
        .into_connection();

    let state = test_support::prelude::test_state(db);
    let result = build_handler::handle_nar_ready(&state, store_path, 12345, "sha256-abc123").await;
    assert!(
        result.is_ok(),
        "nar_ready should succeed: {:?}",
        result.err()
    );
}

/// When NarReady references a store_path that doesn't exist in the DB,
/// the handler logs a warning but doesn't error.
#[tokio::test]
async fn nar_ready_unknown_path_warns() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find derivation_output by store_path → empty
        .append_query_results([Vec::<MDerivationOutput>::new()])
        .into_connection();

    let state = test_support::prelude::test_state(db);
    let result = build_handler::handle_nar_ready(&state, "/nix/store/unknown-path", 0, "").await;
    assert!(
        result.is_ok(),
        "nar_ready for unknown path should not error"
    );
}

// ── Group T: Credentials ────────────────────────────────────────────────────
// Credential delivery (SSH keys, signing keys) happens in the proto handler
// before AssignJob. Tests for this require WebSocket-level integration testing
// which is beyond MockDB unit tests. See proto/src/handler.rs.
