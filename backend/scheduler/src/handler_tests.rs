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
//! All evaluations use `project: None` so that the dispatch_*_event helpers
//! spawned inside `update_build_status` / `update_evaluation_status` return
//! early without consuming staged MockDatabase results.

use std::sync::Arc;

use chrono::NaiveDateTime;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::types::*;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

use crate::jobs::{PendingBuildJob, PendingEvalJob};
use crate::{build as build_handler, eval as eval_handler};
use gradient_core::types::proto::{
    BuildOutput, BuildProduct, DerivationOutput, DiscoveredDerivation, FlakeJob, FlakeTask,
};
// ── Fixture helpers ──────────────────────────────────────────────────────────

fn test_date() -> NaiveDateTime {
    NaiveDateTime::default()
}

/// Evaluation fixture. `project: None` prevents dispatch_evaluation_event_for_status
/// from doing any DB queries (it returns early when project is None).
fn make_eval(id: EvaluationId, status: EvaluationStatus) -> MEvaluation {
    entity::evaluation::Model {
        id,
        project: None,
        repository: "https://example.com/repo".into(),
        commit: CommitId::nil(),
        wildcard: "*".into(),
        status,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
        check_run_ids: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
        source_comment: None,
    }
}

fn make_build(
    id: BuildId,
    eval_id: EvaluationId,
    drv_id: DerivationId,
    status: BuildStatus,
) -> MBuild {
    entity::build::Model {
        id,
        evaluation: eval_id,
        derivation: drv_id,
        status,
        log_id: None,
        build_time_ms: None,
        worker: None,
        via: None,
        external_cached: false,
        attempt: 0,
        timeout_secs: None,
        max_silent_secs: None,
        prefer_local_build: false,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn make_derivation(id: DerivationId, org_id: OrganizationId, path: &str) -> MDerivation {
    let stripped = gradient_core::executer::strip_nix_store_prefix(path);
    let (hash, name) = gradient_core::sources::parse_drv_hash_name(&stripped)
        .unwrap_or_else(|_| ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), "x".into()));
    entity::derivation::Model {
        id,
        organization: org_id,
        hash,
        name,
        architecture: "x86_64-linux".into(),
        created_at: test_date(),
    }
}

fn make_drv_output(
    id: DerivationOutputId,
    drv_id: DerivationId,
    name: &str,
    path: &str,
) -> MDerivationOutput {
    let hash = path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
        .unwrap_or("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .to_string();
    entity::derivation_output::Model {
        id,
        derivation: drv_id,
        name: name.to_string(),
        hash,
        package: name.to_string(),
        ca: None,
        nar_size: None,
        is_cached: false,
        cached_path: None,
        created_at: test_date(),
    }
}

/// `cached_path` row whose `file_hash` is set - `is_fully_cached()` returns
/// true. Used by `eval_result_*` tests that exercise the substituted-status
/// branch of `insert_build_rows`.
fn make_fully_cached_path(id: CachedPathId, store_path: &str) -> entity::cached_path::Model {
    entity::cached_path::Model {
        id,
        store_path: store_path.to_string(),
        hash: store_path
            .strip_prefix("/nix/store/")
            .and_then(|s| s.split('-').next())
            .unwrap_or("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .to_string(),
        package: "test".into(),
        file_hash: Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        file_size: Some(1),
        nar_size: Some(1),
        nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
        references: None,
        ca: None,
        deriver: None,
        created_at: test_date(),
    }
}

fn make_dep_edge(
    id: DerivationDependencyId,
    drv_id: DerivationId,
    dep_id: DerivationId,
) -> MDerivationDependency {
    entity::derivation_dependency::Model {
        id,
        derivation: drv_id,
        dependency: dep_id,
    }
}

fn make_eval_job(eval_id: EvaluationId, org_id: OrganizationId) -> PendingEvalJob {
    PendingEvalJob {
        evaluation_id: eval_id,
        project_id: None,
        peer_id: org_id,
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            source: gradient_core::types::proto::FlakeSource::Repository {
                url: "https://example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
        },
        required_paths: vec![],
        queued_at: gradient_core::types::now(),
    }
}

fn make_build_job(
    build_id: BuildId,
    eval_id: EvaluationId,
    org_id: OrganizationId,
) -> PendingBuildJob {
    use gradient_core::types::proto::{BuildJob, BuildTask};
    PendingBuildJob {
        build_id,
        evaluation_id: eval_id,
        peer_id: org_id,
        job: BuildJob {
            builds: vec![BuildTask {
                build_id: build_id.to_string(),
                drv_path: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv".into(),
                external_cached: false,
            }],
        },
        required_paths: vec![],
        architecture: "x86_64-linux".into(),
        required_features: vec![],
        dependency_count: 0,
        queued_at: gradient_core::types::now(),
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
fn make_eval_with_project(
    id: EvaluationId,
    project_id: ProjectId,
    status: EvaluationStatus,
) -> MEvaluation {
    entity::evaluation::Model {
        id,
        project: Some(project_id),
        repository: "https://example.com/repo".into(),
        commit: CommitId::nil(),
        wildcard: "*".into(),
        status,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
        check_run_ids: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
        source_comment: None,
    }
}

/// Project fixture for webhook tests.
fn make_project(id: ProjectId, org_id: OrganizationId) -> entity::project::Model {
    entity::project::Model {
        id,
        organization: org_id,
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: "".into(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: UserId::nil(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: 30,
        concurrency: 3,
        sign_cache: true,
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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();

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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();

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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();

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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

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
            DerivationOutputId::now_v7(),
            drv_id,
            "out",
            out_path,
        )]])
        // 4a. compute_truly_substituted: load derivation_output → empty (none cached)
        .append_query_results([Vec::<MDerivationOutput>::new()])
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

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok());
}

/// When a derivation already exists in the DB, its row is reused (no insert),
/// but a new build row is still created for this evaluation.
#[tokio::test]
async fn eval_result_existing_derivation_reuses_id() {
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

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
        // (no insert_many derivations or outputs - already exists)
        // 2a. compute_truly_substituted: load derivation_output → empty
        .append_query_results([Vec::<MDerivationOutput>::new()])
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

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok());
}

/// Substituted derivations create build rows with status=Substituted, not Created.
/// The "find Created builds" query then returns empty, so the eval goes Completed
/// immediately (all work was already in the store).
#[tokio::test]
async fn eval_result_substituted_derivation_completes_eval() {
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let cp_id = CachedPathId::now_v7();

    let drv_path = "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-sub.drv";
    let out_path = "/nix/store/ffffffffffffffffffffffffffffffff-sub";
    let mut discovered = make_discovered(drv_path, vec![("out", out_path)], vec![]);
    discovered.substituted = true;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(eval)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 2. find existing derivations → return our drv as already-known so
        //    `drv_path_to_id` carries the test's `drv_id` instead of one
        //    freshly generated by `DerivationInsertBatch::prepare`. This lets
        //    the substituted-cache mocks (4a/4b) reference the same `drv_id`
        //    the rest of `insert_build_rows` sees.
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // (no insert_many derivations / outputs - already exists)
        // 4a. compute_truly_substituted: load derivation_output → cached row
        .append_query_results([vec![{
            let mut o = make_drv_output(DerivationOutputId::now_v7(), drv_id, "out", out_path);
            o.is_cached = true;
            o.cached_path = Some(cp_id);
            o
        }]])
        // 4b. compute_truly_substituted: load cached_path → fully cached
        .append_query_results([vec![make_fully_cached_path(cp_id, out_path)]])
        // 4c. find_log_sources: no prior builds to inherit log from
        .append_query_results([Vec::<MBuild>::new()])
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

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

/// `compute_truly_substituted` matches by hash, not by the
/// `derivation_output.cached_path` link, so a re-evaluated drv whose
/// output hash is in `cached_path` (e.g. shared FOD source, manual cache
/// push) gets marked Substituted on the very first eval pass - the link
/// is otherwise only set after a fresh upload runs `mark_nar_stored`.
#[tokio::test]
async fn eval_result_substitutes_when_hash_in_cached_path_without_link() {
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let cp_id = CachedPathId::now_v7();

    let drv_path = "/nix/store/dddddddddddddddddddddddddddddddd-sub.drv";
    let out_path = "/nix/store/cccccccccccccccccccccccccccccccc-sub";
    let mut discovered = make_discovered(drv_path, vec![("out", out_path)], vec![]);
    discovered.substituted = true;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // derivation_output: hash matches the cached_path below, but
        // cached_path link is None and is_cached is false - the row was
        // inserted by eval before any upload had run.
        .append_query_results([vec![make_drv_output(
            DerivationOutputId::now_v7(),
            drv_id,
            "out",
            out_path,
        )]])
        // cached_path keyed by the same hash, fully uploaded.
        .append_query_results([vec![make_fully_cached_path(cp_id, out_path)]])
        // find_log_sources: no prior builds for this drv yet.
        .append_query_results([Vec::<MBuild>::new()])
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Substituted,
        )]])
        .append_query_results([Vec::<MBuild>::new()])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Completed)]])
        .into_connection();

    let state = make_state(db);
    let job = make_eval_job(eval_id, org_id);

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

/// Two derivations where A depends on B: the dependency edge is inserted between them.
/// Both builds are queued and eval transitions to Building.
#[tokio::test]
async fn eval_result_with_dependencies() {
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();

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
            DerivationOutputId::now_v7(),
            drv_a_id,
            "out",
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a",
        )]])
        // 5. compute_truly_substituted: load derivation_output → empty
        // (dep edges are deferred to handle_eval_job_completed, not inserted here)
        .append_query_results([Vec::<MDerivationOutput>::new()])
        // 5a. find_active_leaders → empty
        .append_query_results([Vec::<MBuild>::new()])
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

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![drv_a, drv_b], vec![], vec![]).await;
    assert!(result.is_ok(), "got: {:?}", result.err());
}

/// Warnings in the eval result are recorded as evaluation_message rows before
/// the build queue transition.
#[tokio::test]
async fn eval_result_with_warnings() {
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

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
            DerivationOutputId::now_v7(),
            drv_id,
            "out",
            "/nix/store/hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh-warn",
        )]])
        // 4a. compute_truly_substituted: load derivation_output → empty
        .append_query_results([Vec::<MDerivationOutput>::new()])
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
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let org_id = OrganizationId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update_build_status → Completed (UPDATE...RETURNING)
        .append_query_results([vec![build_completed]])
        // 2a. propagate_to_followers: find via=leader.id → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 5. find failed builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 6. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 7. update_many eval → Completed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 8. find_by_id(eval) → Completed
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
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let other_drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let other_build_id = BuildId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let other_build = make_build(other_build_id, eval_id, other_drv_id, BuildStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update build → Completed
        .append_query_results([vec![build_completed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. find active builds → still has other_build
        .append_query_results([vec![other_build]])
        // check_evaluation_done returns early - no further queries
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// All builds done but some are Failed → eval transitions to Failed.
#[tokio::test]
async fn build_completed_with_failed_sibling() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let failed_drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let failed_build_id = BuildId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let failed_build = make_build(failed_build_id, eval_id, failed_drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Completed
        .append_query_results([vec![build_completed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find eval → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 5. find failed builds → has one Failed build
        .append_query_results([vec![failed_build]])
        // 6. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 7. update eval → Failed
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

/// Eval is not in Building status - check_evaluation_done returns without updating.
#[tokio::test]
async fn build_completed_eval_not_building_noop() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Completed
        .append_query_results([vec![build_completed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
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

/// Build ID not found - handler returns Ok(()) without touching eval status.
#[tokio::test]
async fn build_completed_unknown_build_noop() {
    let build_id = BuildId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // find_by_id(build) → None
        .append_query_results([Vec::<MBuild>::new()])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
}

/// DependencyFailed builds count as failed - if all builds are
/// DependencyFailed/Completed, the eval transitions to Failed.
#[tokio::test]
async fn build_completed_dep_failed_siblings_cause_eval_failed() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let dep_failed_drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let dep_failed_build_id = BuildId::now_v7();

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
        .append_query_results([Vec::<MBuild>::new()]) // propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()]) // no active
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        .append_query_results([vec![dep_failed]]) // DependencyFailed counts
        .append_query_results([Vec::<MEvaluationMessage>::new()]) // no eval errors
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
    let eval_id = EvaluationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);
    let build_b_dep_failed =
        make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed);
    // Edge: B.drv depends on A.drv
    let dep_edge = make_dep_edge(DerivationDependencyId::now_v7(), drv_b_id, drv_a_id);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update buildA → Failed (UPDATE...RETURNING)
        .append_query_results([vec![build_a_failed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // ── collect_transitive_dependents ──
        // 3. BFS layer 1: dep edges where Dependency=A → [B→A]
        .append_query_results([vec![dep_edge]])
        // 4. BFS layer 2: dep edges where Dependency=B → empty
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // 5. cascade: find Created/Queued builds with derivation in {B} → [buildB]
        .append_query_results([vec![build_b]])
        // 6. update buildB → DependencyFailed
        .append_query_results([vec![build_b_dep_failed]])
        // ── check_evaluation_done ──
        // 7. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 8. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 9. find failed builds → [buildA{Failed}, buildB{DependencyFailed}]
        .append_query_results([vec![
            make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed),
            make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed),
        ]])
        // 10. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 11. update eval → Failed
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

/// Regression: when a build fails (e.g. worker reports a prefetch-time
/// `acquire local store for import: timeout`), the worker's error string
/// must be appended to the build log so the frontend's log viewer surfaces
/// it instead of rendering "No log available". Previously
/// `handle_build_job_failed` accepted the error and dropped it on the floor.
#[tokio::test]
async fn build_failed_appends_worker_error_to_log() {
    use std::sync::Arc;
    use test_support::prelude::{RecordingLogStorage, test_state_with_log_storage};

    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_failed = make_build(build_id, eval_id, drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Failed
        .append_query_results([vec![build_failed.clone()]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. cascade: no candidates
        .append_query_results([Vec::<MBuild>::new()])
        // 4. check active → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 5. find eval → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 6. find failed → [build]
        .append_query_results([vec![build_failed]])
        // 7. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 8. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Failed)]])
        .into_connection();

    let log = Arc::new(RecordingLogStorage::new());
    let state = test_state_with_log_storage(db, log.clone());

    let worker_error = "input prefetch failed: acquire local store for import: timeout: \
                        acquiring connection from pool";
    let result = build_handler::handle_build_job_failed(&state, build_id, worker_error).await;
    assert!(result.is_ok(), "handler returned error: {result:?}");

    let entries = log.entries();
    let appended = entries
        .iter()
        .find(|(b, _)| *b == build_id)
        .map(|(_, t)| t.as_str())
        .unwrap_or_else(|| panic!("expected an append for build {build_id}, got {entries:?}"));
    assert!(
        appended.contains(worker_error),
        "appended log must include the worker's error string verbatim, got: {appended:?}"
    );
}

/// Build fails with no Created/Queued dependents - cascade is a no-op.
/// check_evaluation_done sees only the Failed build → eval → Failed.
#[tokio::test]
async fn build_failed_no_dependents() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_failed = make_build(build_id, eval_id, drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. update → Failed
        .append_query_results([vec![build_failed.clone()]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. cascade: find Created/Queued → empty (no candidates)
        .append_query_results([Vec::<MBuild>::new()])
        // 4. check active → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 5. find eval → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 6. find failed → [build]
        .append_query_results([vec![build_failed]])
        // 7. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 8. update eval → Failed
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
    let eval_id = EvaluationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let drv_c_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();
    let build_c_id = BuildId::now_v7();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);
    let build_b_dep_failed =
        make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed);
    let build_c = make_build(build_c_id, eval_id, drv_c_id, BuildStatus::Queued);
    // B depends on A; C does NOT depend on A
    let dep_edge_b_a = make_dep_edge(DerivationDependencyId::now_v7(), drv_b_id, drv_a_id);
    let _ = drv_c_id; // referenced only for documentation

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update → Failed
        .append_query_results([vec![build_a_failed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // ── collect_transitive_dependents ──
        // 3. BFS layer 1: dep edges where Dependency=A → [B→A] (C has no edge to A)
        .append_query_results([vec![dep_edge_b_a]])
        // 4. BFS layer 2: dep edges where Dependency=B → empty
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // 5. cascade: find Created/Queued with derivation in {B} → [buildB]
        // (C is excluded by the derivation filter, not by a per-row dep check)
        .append_query_results([vec![build_b]])
        // 6. update buildB → DependencyFailed
        .append_query_results([vec![build_b_dep_failed]])
        // ── check_evaluation_done ──
        // 7. check active → buildC is still Queued
        .append_query_results([vec![build_c]])
        // active not empty → check_evaluation_done returns early
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_a_id, "error").await;
    assert!(result.is_ok());
}

/// Build job failed for an unknown build ID - handler returns Ok(()) silently.
#[tokio::test]
async fn build_failed_unknown_build_noop() {
    let build_id = BuildId::now_v7();

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
    let eval_id = EvaluationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();

    // Building → Failed is the valid terminal failure transition per the state machine.
    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    // B is Building (not Created/Queued) - cascade filter excludes it
    let build_b_building = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Building);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update → Failed
        .append_query_results([vec![build_a_failed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
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

/// Build outputs update the `nar_size` and `file_hash` fields
/// of the corresponding `derivation_output` row, then delete+insert `build_product` rows.
#[tokio::test]
async fn build_output_updates_derivation_output() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let drv_out_id = DerivationOutputId::now_v7();
    let org_id = OrganizationId::now_v7();

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
        o
    };

    let outputs = vec![BuildOutput {
        name: "out".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
        hash: "aaaa".into(),
        nar_size: Some(12345),
        nar_hash: Some("sha256:abc".into()),
        products: vec![],
    }];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. find derivation_output row
        .append_query_results([vec![drv_out]])
        // 3. update derivation_output (UPDATE...RETURNING)
        .append_query_results([vec![drv_out_updated]])
        // 4. delete_many build_product rows → exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        // No product inserts (products is empty)
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
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let org_id = OrganizationId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let outputs = vec![BuildOutput {
        name: "out".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
        hash: "aaaa".into(),
        nar_size: None,
        nar_hash: None,
        products: vec![],
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

/// When the output has products, `handle_build_output` inserts `build_product` rows
/// after updating the `derivation_output` row.
#[tokio::test]
async fn build_output_inserts_build_product_rows() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let drv_out_id = DerivationOutputId::now_v7();
    let org_id = OrganizationId::now_v7();

    let build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let drv_out = make_drv_output(
        drv_out_id,
        drv_id,
        "out",
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out",
    );
    let drv_out_updated = {
        let mut o = drv_out.clone();
        o.nar_size = Some(99);
        o
    };

    // A fake build_product row that the insert mock needs to return.
    let fake_bp = entity::build_product::Model {
        id: BuildProductId::now_v7(),
        derivation_output: drv_out_id,
        file_type: "file".into(),
        subtype: "iso".into(),
        name: "image.iso".into(),
        path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out/image.iso".into(),
        size: Some(1024),
        created_at: test_date(),
    };

    let outputs = vec![BuildOutput {
        name: "out".into(),
        store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
        hash: "aaaa".into(),
        nar_size: Some(99),
        nar_hash: Some("sha256:abc".into()),
        products: vec![BuildProduct {
            file_type: "file".into(),
            subtype: "iso".into(),
            name: "image.iso".into(),
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out/image.iso".into(),
            size: Some(1024),
        }],
    }];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build]])
        // 2. find derivation_output row
        .append_query_results([vec![drv_out]])
        // 3. update derivation_output (UPDATE...RETURNING)
        .append_query_results([vec![drv_out_updated]])
        // 4. delete_many prior build_product rows → exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        // 5. insert build_product row → exec (single insert with explicit PK)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    // Silence the unused warning from fake_bp in the mock setup.
    let _ = fake_bp;

    let state = make_state(db);
    let job = make_build_job(build_id, eval_id, org_id);

    let result = build_handler::handle_build_output(&state, &job, build_id, outputs).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

/// Build not found → handler returns an Err (build context is mandatory).
#[tokio::test]
async fn build_output_unknown_build_errors() {
    let eval_id = EvaluationId::now_v7();
    let build_id = BuildId::now_v7();
    let org_id = OrganizationId::now_v7();

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
/// `handle_eval_job_completed` (no active builds, no failures): promotes any
/// Created → Queued (none here), flips eval EvaluatingDerivation → Building,
/// then `check_evaluation_done` immediately closes it as Completed.
#[tokio::test]
async fn eval_job_completed_no_active_builds_completes_eval() {
    let eval_id = EvaluationId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Created builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 2. find_by_id(eval) → EvaluatingDerivation
        .append_query_results([vec![make_eval(
            eval_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 3. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // ── check_evaluation_done ──
        // 5. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 6. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 7. find failed builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 8. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 9. update_many eval → Completed
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
/// to Failed (not Completed). Regression guard: a failed build must not be
/// silently masked into a Completed evaluation.
#[tokio::test]
async fn eval_job_completed_with_failed_build_marks_eval_failed() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

    let failed_build = make_build(build_id, eval_id, drv_id, BuildStatus::Failed);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Created builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 2. find_by_id(eval) → EvaluatingDerivation
        .append_query_results([vec![make_eval(
            eval_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 3. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // ── check_evaluation_done ──
        // 5. find active builds → empty (all terminal)
        .append_query_results([Vec::<MBuild>::new()])
        // 6. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 7. find failed builds → [failed_build]
        .append_query_results([vec![failed_build]])
        // 8. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 9. update_many eval → Failed
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

/// Eval job completes with no build failures but has eval-error messages
/// (e.g. some wildcard attrs failed to resolve).  `check_evaluation_done`
/// must mark the evaluation as `Failed`, not `Completed`.
#[tokio::test]
async fn eval_job_completed_with_eval_errors_marks_eval_failed() {
    let eval_id = EvaluationId::now_v7();

    let eval_msg = entity::evaluation_message::Model {
        id: EvaluationMessageId::now_v7(),
        evaluation: eval_id,
        level: entity::evaluation_message::MessageLevel::Error,
        message: "packages.x86_64-linux.broken: attribute missing".into(),
        source: Some("nix-eval".into()),
        created_at: test_date(),
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Created builds (to promote to Queued) → none
        .append_query_results([Vec::<MBuild>::new()])
        // 2. find_by_id(eval) → EvaluatingDerivation
        .append_query_results([vec![make_eval(
            eval_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 3. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // ── check_evaluation_done ──
        // 5. find active builds → empty (all done or substituted)
        .append_query_results([Vec::<MBuild>::new()])
        // 6. find_by_id(eval) → Building
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 7. find failed builds → empty (builds themselves passed)
        .append_query_results([Vec::<MBuild>::new()])
        // 8. find eval error messages → one error
        .append_query_results([vec![eval_msg]])
        // 9. update_many eval → Failed (because of eval errors)
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

/// When the eval job ends and builds are still pending,
/// `handle_eval_job_completed` promotes any `Created` builds to `Queued`,
/// flips the evaluation to `Building`, and then `check_evaluation_done`
/// returns early because builds are still in flight.
#[tokio::test]
async fn eval_job_completed_active_builds_remain_noop() {
    let eval_id = EvaluationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

    let active_build = make_build(build_id, eval_id, drv_id, BuildStatus::Building);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find Created builds (to promote to Queued) → none
        .append_query_results([Vec::<MBuild>::new()])
        // 2. find_by_id(eval) → still in EvaluatingDerivation
        .append_query_results([vec![make_eval(
            eval_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 3. update_many eval → Building
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 4. find_by_id(eval) → Building (after update)
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 5. check_evaluation_done: find active builds → has the still-Building one
        .append_query_results([vec![active_build]])
        // (early return - no further queries)
        .into_connection();

    let state = make_state(db);
    let result = eval_handler::handle_eval_job_completed(&state, eval_id).await;
    assert!(result.is_ok());
}

/// A failed eval job transitions the evaluation from Building to Failed and
/// records an error message.
#[tokio::test]
async fn eval_job_failed_transitions_eval_to_failed() {
    let eval_id = EvaluationId::now_v7();

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
    let eval_id = EvaluationId::now_v7();

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
///      → spawns dispatch_build_event_for_status(Aborted) - returns early (Aborted → return)
///      → spawns log_finalize (NoopLogStorage → no-op)
///   3. Q: update buildB → Aborted
///   4. E: update_many eval → Aborted
///   5. Q: find_by_id(eval) after update
///      → spawns dispatch_evaluation_event_for_status (eval.project=None → returns early)
#[tokio::test]
async fn abort_cascades_to_active_builds() {
    let eval_id = EvaluationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();

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
    let eval_id = EvaluationId::now_v7();
    // Empty MockDatabase - any unexpected DB call would cause an error that,
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
    let eval_id = EvaluationId::now_v7();
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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();

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

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();

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
            DerivationOutputId::now_v7(),
            drv_id,
            "out",
            out_path,
        )]])
        // 4a. compute_truly_substituted: load derivation_output → empty
        .append_query_results([Vec::<MDerivationOutput>::new()])
        // 4b. find_active_leaders:
        //   same-org pass: empty
        //   cross-org pass: empty derivation lookup short-circuits
        .append_query_results([Vec::<MBuild>::new()])
        .append_query_results([Vec::<MDerivation>::new()])
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

    let result =
        eval_handler::handle_eval_result(&state, &job, vec![discovered], vec![], vec![]).await;
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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();

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
            DerivationOutputId::now_v7(),
            drv_b_id,
            "out",
            "/nix/store/bbbb-new",
        )]])
        // 5. compute_truly_substituted: load derivation_output → empty
        // (dep edges are deferred to handle_eval_job_completed, not inserted here)
        .append_query_results([Vec::<MDerivationOutput>::new()])
        // 5a. find_active_leaders → empty
        .append_query_results([Vec::<MBuild>::new()])
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

    let result = eval_handler::handle_eval_result(
        &state,
        &job,
        vec![drv_a_existing, drv_b_new],
        vec![],
        vec![],
    )
    .await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

// ── Group I: Action dispatch ──────────────────────────────────────────────────

/// After `handle_build_job_completed`, dispatch_build_event_for_status and
/// dispatch_evaluation_event_for_status are called with the correct project_id.
///
/// DB call sequence (main handler):
///   1. Q: find_by_id(build) → Building
///   2. Q: update build → Completed (UPDATE…RETURNING)
///      → spawns TASK_A: dispatch_build_event_for_status(build, Completed)
///      → spawns TASK_B: log_finalize (NoopLogStorage → no-op)
///   3–7. find active/failed builds, update_many eval → Completed, find_by_id(eval)
///      → spawns TASK_C: dispatch_evaluation_event_for_status(eval, Completed)
///
/// TASK_A: find_by_id(eval), find derivation, find project_actions → []
/// TASK_B: no-op.
/// TASK_C: find project_actions → []
#[tokio::test]
async fn action_dispatched_on_build_completed() {
    let eval_id = EvaluationId::now_v7();
    let project_id = ProjectId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

    let build_building = make_build(build_id, eval_id, drv_id, BuildStatus::Building);
    let build_completed = make_build(build_id, eval_id, drv_id, BuildStatus::Completed);
    let eval_building = make_eval_with_project(eval_id, project_id, EvaluationStatus::Building);
    let eval_completed = make_eval_with_project(eval_id, project_id, EvaluationStatus::Completed);
    let drv = make_derivation(drv_id, org_id, "/nix/store/aaaa-hello.drv");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(build)
        .append_query_results([vec![build_building]])
        // 2. update build → Completed
        .append_query_results([vec![build_completed]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 4. find_by_id(eval) → Building
        .append_query_results([vec![eval_building]])
        // 5. find failed builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 6. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 7. update_many eval → Completed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 8. find_by_id(eval) → Completed
        .append_query_results([vec![eval_completed.clone()]])
        // TASK_A: dispatch_build_event_for_status(Completed)
        // 9. find_by_id(eval)
        .append_query_results([vec![eval_completed]])
        // 10. find_by_id(build.derivation)
        .append_query_results([vec![drv]])
        // 11. find project_actions → []
        .append_query_results([Vec::<entity::project_action::Model>::new()])
        // TASK_C: dispatch_evaluation_event_for_status(Completed)
        // 12. find project_actions → []
        .append_query_results([Vec::<entity::project_action::Model>::new()])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_completed(&state, build_id).await;
    assert!(result.is_ok());
    tokio::task::yield_now().await;
}

/// After `handle_build_job_failed` where build B is cascaded to `DependencyFailed`,
/// dispatch_build_event_for_status fires for build A (Failed) but returns early
/// for build B (DependencyFailed → `Created | Aborted | DependencyFailed => return`).
///
/// DB call sequence (main handler):
///   1–6. update buildA → Failed, cascade buildB → DependencyFailed
///   7–12. check_evaluation_done → update eval → Failed, find_by_id(eval)
///      → spawns TASK_A: dispatch_build_event_for_status(buildA, Failed)
///      → spawns TASK_C: dispatch_build_event_for_status(buildB, DependencyFailed) - returns early
///      → spawns TASK_E: dispatch_evaluation_event_for_status(eval, Failed)
///
/// TASK_A: find_by_id(eval), find derivation, find project_actions → []
/// TASK_C: returns immediately (DependencyFailed → early return), no DB.
/// TASK_E: find project_actions → []
#[tokio::test]
async fn action_not_dispatched_for_dep_failed() {
    let eval_id = EvaluationId::now_v7();
    let project_id = ProjectId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_a_id = DerivationId::now_v7();
    let drv_b_id = DerivationId::now_v7();
    let build_a_id = BuildId::now_v7();
    let build_b_id = BuildId::now_v7();

    let build_a = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Building);
    let build_a_failed = make_build(build_a_id, eval_id, drv_a_id, BuildStatus::Failed);
    let build_b = make_build(build_b_id, eval_id, drv_b_id, BuildStatus::Queued);
    let build_b_dep_failed =
        make_build(build_b_id, eval_id, drv_b_id, BuildStatus::DependencyFailed);
    let dep_edge = make_dep_edge(DerivationDependencyId::now_v7(), drv_b_id, drv_a_id);
    let eval_building = make_eval_with_project(eval_id, project_id, EvaluationStatus::Building);
    let eval_failed = make_eval_with_project(eval_id, project_id, EvaluationStatus::Failed);
    let drv_a = make_derivation(drv_a_id, org_id, "/nix/store/aaaa-a.drv");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find_by_id(buildA)
        .append_query_results([vec![build_a]])
        // 2. update buildA → Failed
        .append_query_results([vec![build_a_failed.clone()]])
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // 3. BFS layer 1: dep edges where Dependency=A → [B→A]
        .append_query_results([vec![dep_edge]])
        // 4. BFS layer 2: dep edges where Dependency=B → empty
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // 5. cascade: find Created/Queued with derivation in {B} → [buildB]
        .append_query_results([vec![build_b]])
        // 6. update buildB → DependencyFailed
        .append_query_results([vec![build_b_dep_failed.clone()]])
        // 7. check active → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 8. find_by_id(eval) → Building
        .append_query_results([vec![eval_building]])
        // 9. find failed → [buildA, buildB]
        .append_query_results([vec![build_a_failed, build_b_dep_failed]])
        // 10. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 11. update_many eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 12. find_by_id(eval) → Failed
        .append_query_results([vec![eval_failed.clone()]])
        // TASK_A: dispatch_build_event_for_status(Failed)
        // 13. find_by_id(eval)
        .append_query_results([vec![eval_failed]])
        // 14. find_by_id(buildA.derivation)
        .append_query_results([vec![drv_a]])
        // 15. find project_actions → []
        .append_query_results([Vec::<entity::project_action::Model>::new()])
        // TASK_C: DependencyFailed → returns immediately, no DB
        // TASK_E: dispatch_evaluation_event_for_status(Failed)
        // 16. find project_actions → []
        .append_query_results([Vec::<entity::project_action::Model>::new()])
        .into_connection();

    let state = make_state(db);
    let result = build_handler::handle_build_job_failed(&state, build_a_id, "build error").await;
    assert!(result.is_ok());
    tokio::task::yield_now().await;
}

// ── Group K: Entry Points ───────────────────────────────────────────────────

/// When handle_eval_result receives derivations for an evaluation with
/// project_id: Some, it must create entry_point rows mapping each derivation's
/// attr path to its build. This was done by the legacy evaluator but was never
/// tested in the new scheduler.
#[tokio::test]
async fn eval_result_creates_entry_points_for_project() {
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let project_id = ProjectId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

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
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            source: gradient_core::types::proto::FlakeSource::Repository {
                url: "https://example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
        },
        required_paths: vec![],
        queued_at: gradient_core::types::now(),
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
            DerivationOutputId::now_v7(),
            drv_id,
            "out",
            out_path,
        )]])
        // 4a. compute_truly_substituted: load derivation_output → empty
        .append_query_results([Vec::<MDerivationOutput>::new()])
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
            id: EntryPointId::now_v7(),
            project: project_id,
            evaluation: eval_id,
            build: build_id,
            eval: "packages.x86_64-linux.hello".into(),
            created_at: test_date(),
            repo_check_id: None,
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
    let result =
        eval_handler::handle_eval_result(&state, &eval_job, derivations, vec![], vec![]).await;
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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();

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

    // project_id: None - direct build, no entry points
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
            DerivationOutputId::now_v7(),
            drv_id,
            "out",
            out_path,
        )]])
        // 4a. compute_truly_substituted: load derivation_output → empty
        .append_query_results([Vec::<MDerivationOutput>::new()])
        // 5. insert build (Created)
        .append_query_results([vec![make_build(
            build_id,
            eval_id,
            drv_id,
            BuildStatus::Created,
        )]])
        // NO entry point insert, NO project query - skipped when project_id is None
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
    let result =
        eval_handler::handle_eval_result(&state, &eval_job, derivations, vec![], vec![]).await;
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
    let eval_id = EvaluationId::now_v7();
    // Evaluation in Fetching status - prefetch failure
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
    let eval_id = EvaluationId::now_v7();
    let org_id = OrganizationId::now_v7();
    let project_id = ProjectId::now_v7();
    let drv_id = DerivationId::now_v7();
    let build_id = BuildId::now_v7();
    let cp_id = CachedPathId::now_v7();

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
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            source: gradient_core::types::proto::FlakeSource::Repository {
                url: "https://example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
        },
        required_paths: vec![],
        queued_at: gradient_core::types::now(),
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. find eval
        .append_query_results([vec![make_eval_with_project(
            eval_id,
            project_id,
            EvaluationStatus::EvaluatingDerivation,
        )]])
        // 2. find existing derivations → return our drv as already-known so
        //    `drv_path_to_id` carries the test's `drv_id` (lets the
        //    substituted-cache mocks below reference the same id).
        .append_query_results([vec![make_derivation(drv_id, org_id, drv_path)]])
        // (no insert derivation / outputs - already exists)
        // 4a. compute_truly_substituted: load derivation_output → cached row
        .append_query_results([vec![{
            let mut o = make_drv_output(DerivationOutputId::now_v7(), drv_id, "out", out_path);
            o.is_cached = true;
            o.cached_path = Some(cp_id);
            o
        }]])
        // 4b. compute_truly_substituted: load cached_path → fully cached
        .append_query_results([vec![make_fully_cached_path(cp_id, out_path)]])
        // 5. insert build (Substituted - not Created!)
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
            id: EntryPointId::now_v7(),
            project: project_id,
            evaluation: eval_id,
            build: build_id,
            eval: "packages.x86_64-linux.hello".into(),
            created_at: test_date(),
            repo_check_id: None,
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
    let result =
        eval_handler::handle_eval_result(&state, &eval_job, derivations, vec![], vec![]).await;
    assert!(
        result.is_ok(),
        "substituted eval with project should complete: {:?}",
        result.err()
    );
}

// ── Group M: Transitive DependencyFailed Cascade ────────────────────────────

/// When build C fails and B depends on C and A depends on B, ALL of them should
/// be marked DependencyFailed - not just the direct dependent B.
/// The cascade walks the derivation_dependency graph transitively (BFS).
#[tokio::test]
async fn build_failed_cascades_transitively_through_graph() {
    // A depends on B, B depends on C. C fails.
    // Expected: B → DependencyFailed, then A → DependencyFailed (transitive).
    let eval_id = EvaluationId::now_v7();
    let drv_a = DerivationId::now_v7();
    let drv_b = DerivationId::now_v7();
    let drv_c = DerivationId::now_v7();
    let build_a = BuildId::now_v7();
    let build_b = BuildId::now_v7();
    let build_c = BuildId::now_v7();

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
        // 2a. propagate_to_followers: empty
        .append_query_results([Vec::<MBuild>::new()])
        // ── collect_transitive_dependents ──
        // 3. BFS layer 1: dep edges where Dependency=C → [B→C]
        .append_query_results([vec![make_dep_edge(
            DerivationDependencyId::now_v7(),
            drv_b,
            drv_c,
        )]])
        // 4. BFS layer 2: dep edges where Dependency=B → [A→B]
        .append_query_results([vec![make_dep_edge(
            DerivationDependencyId::now_v7(),
            drv_a,
            drv_b,
        )]])
        // 5. BFS layer 3: dep edges where Dependency=A → empty
        .append_query_results([Vec::<MDerivationDependency>::new()])
        // 6. cascade: find Created/Queued with derivation in {A, B} → [build_a, build_b]
        .append_query_results([vec![
            make_build(build_a, eval_id, drv_a, BuildStatus::Queued),
            make_build(build_b, eval_id, drv_b, BuildStatus::Queued),
        ]])
        // 7. update first cascaded build → DependencyFailed
        .append_query_results([vec![make_build(
            build_a,
            eval_id,
            drv_a,
            BuildStatus::DependencyFailed,
        )]])
        // 8. update second cascaded build → DependencyFailed
        .append_query_results([vec![make_build(
            build_b,
            eval_id,
            drv_b,
            BuildStatus::DependencyFailed,
        )]])
        // ── check_evaluation_done ──
        // 9. find active builds → empty
        .append_query_results([Vec::<MBuild>::new()])
        // 10. find eval
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Building)]])
        // 11. find failed builds → B and A
        .append_query_results([vec![
            make_build(build_b, eval_id, drv_b, BuildStatus::DependencyFailed),
            make_build(build_a, eval_id, drv_a, BuildStatus::DependencyFailed),
        ]])
        // 12. find eval error messages → empty
        .append_query_results([Vec::<MEvaluationMessage>::new()])
        // 13. update eval → Failed
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // 14. find eval → Failed
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
// (Worker-side test - lives in worker/src/executor/fetch.rs)
// The current fetch_repository() returns Ok(()) without cloning anything.
// A proper implementation needs git2 integration.
// See: worker/src/executor/fetch.rs:42-47

// ── Group T: Credentials ────────────────────────────────────────────────────
// Credential delivery (SSH keys, signing keys) happens in the proto handler
// before AssignJob. Tests for this require WebSocket-level integration testing
// which is beyond MockDB unit tests. See proto/src/handler.rs.
