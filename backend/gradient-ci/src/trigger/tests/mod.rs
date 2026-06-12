/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::restart::restart_build_status;
use super::*;
use gradient_types::*;
use fixtures::{make_build, make_build_drv, make_eval, make_project};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{self, EvaluationStatus};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

#[tokio::test]
async fn trigger_creates_queued_eval() {
    let project = make_project();
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1st SELECT: no in-progress evaluations
        .append_query_results([Vec::<evaluation::Model>::new()])
        // INSERT commit → returns commit row
        .append_query_results([vec![gradient_entity::commit::Model {
            id: commit_id,
            hash: vec![0u8; 20],
            ..Default::default()
        }]])
        // INSERT evaluation → returns evaluation row
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Queued)]])
        // SELECT project flake input overrides for snapshot (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // SELECT project for update
        .append_query_results([vec![project.clone()]])
        // UPDATE project → exec result
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result =
        trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None, None).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
    assert_eq!(result.unwrap().status, EvaluationStatus::Queued);
}

#[tokio::test]
async fn trigger_drops_dangling_last_evaluation_pointer() {
    // Project points at an evaluation row that no longer exists. The
    // resolved `previous` must fall back to None so the FK doesn't fire.
    let stale_eval_id = EvaluationId::now_v7();
    let mut project = make_project();
    project.last_evaluation = Some(stale_eval_id);

    let new_eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // in-progress check: none active
        .append_query_results([Vec::<evaluation::Model>::new()])
        // resolve previous: row missing
        .append_query_results([Vec::<evaluation::Model>::new()])
        // insert commit
        .append_query_results([vec![gradient_entity::commit::Model {
            id: commit_id,
            hash: vec![0u8; 20],
            ..Default::default()
        }]])
        // insert evaluation (previous should be None despite stale pointer)
        .append_query_results([vec![make_eval(new_eval_id, EvaluationStatus::Queued)]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // project update read-back + exec
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result =
        trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None, None).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

#[tokio::test]
async fn trigger_already_in_progress() {
    let project = make_project();
    let existing_eval = make_eval(EvaluationId::now_v7(), EvaluationStatus::Queued);

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1st SELECT: returns in-progress evaluation
        .append_query_results([vec![existing_eval]])
        .into_connection();

    let result =
        trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None, None).await;
    assert!(matches!(result, Err(TriggerError::AlreadyInProgress)));
}

#[tokio::test]
async fn trigger_each_active_status_blocks() {
    let active_statuses = [
        EvaluationStatus::Fetching,
        EvaluationStatus::EvaluatingFlake,
        EvaluationStatus::EvaluatingDerivation,
        EvaluationStatus::Building,
        EvaluationStatus::Waiting,
    ];

    for status in active_statuses {
        let project = make_project();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![make_eval(EvaluationId::now_v7(), status)]])
            .into_connection();
        let result =
            trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None, None)
                .await;
        assert!(
            matches!(result, Err(TriggerError::AlreadyInProgress)),
            "{status:?} should block trigger"
        );
    }
}

// ── restart_build_status ─────────────────────────────────────────────────

#[test]
fn restart_status_cached_stays_substituted() {
    assert_eq!(
        restart_build_status(BuildStatus::Completed),
        BuildStatus::Substituted,
    );
    assert_eq!(
        restart_build_status(BuildStatus::Substituted),
        BuildStatus::Substituted,
    );
}

#[test]
fn restart_status_others_become_queued() {
    for s in [
        BuildStatus::Queued,
        BuildStatus::Building,
        BuildStatus::FailedPermanent,
        BuildStatus::FailedTransient,
        BuildStatus::FailedTimeout,
        BuildStatus::Aborted,
        BuildStatus::Created,
        BuildStatus::DependencyFailed,
    ] {
        assert_eq!(
            restart_build_status(s),
            BuildStatus::Queued,
            "{s:?} should be re-queued"
        );
    }
}

#[tokio::test]
async fn trigger_terminal_does_not_block() {
    let project = make_project();
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();

    // Terminal status in DB should not block a new trigger
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1st SELECT: returns completed evaluation (terminal → not in-progress)
        .append_query_results([Vec::<evaluation::Model>::new()])
        .append_query_results([vec![gradient_entity::commit::Model {
            id: commit_id,
            hash: vec![0u8; 20],
            ..Default::default()
        }]])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Queued)]])
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result =
        trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None, None).await;
    assert!(result.is_ok(), "terminal eval should not block new trigger");
}

#[tokio::test]
async fn trigger_records_trigger_id() {
    let project = make_project();
    let trig = ProjectTriggerId::now_v7();
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<evaluation::Model>::new()])
        .append_query_results([vec![gradient_entity::commit::Model {
            id: commit_id,
            hash: vec![0u8; 20],
            ..Default::default()
        }]])
        .append_query_results([vec![{
            let mut m = make_eval(eval_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m
        }]])
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result = trigger_evaluation(
        &db,
        &project,
        vec![0u8; 20],
        None,
        None,
        Some(trig),
        false,
        None,
        None,
        None,
        None,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().trigger, Some(trig));
}

// ── trigger_restart_builds ───────────────────────────────────────────────

/// Regression for the "evaluations stuck in Building forever" symptom:
/// when every previous build is `Completed`/`Substituted`,
/// `restart_build_status` maps them all to `Substituted` (terminal) and
/// no build job is ever dispatched. The new evaluation must therefore
/// start in `Completed`, not `Building`, otherwise nothing fires
/// `check_evaluation_done` and the row is stuck.
#[tokio::test]
async fn restart_with_all_cached_inserts_completed_eval() {
    let project = make_project();
    let prev_eval_id = EvaluationId::now_v7();
    let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Completed);
    let new_eval_id = EvaluationId::now_v7();

    let prev_builds = vec![
        make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Completed),
        make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Substituted),
        make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Completed),
    ];

    let inserted_eval = {
        let mut e = make_eval(new_eval_id, EvaluationStatus::Completed);
        e.previous = Some(prev_eval_id);
        e
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1. in-progress check: none
        .append_query_results([Vec::<evaluation::Model>::new()])
        // 2. find prev_eval
        .append_query_results([vec![prev_eval]])
        // 3. load prev_builds (all terminal)
        .append_query_results([prev_builds])
        // 4. INSERT new evaluation → returns the row with status=Completed
        .append_query_results([vec![inserted_eval]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // 5. INSERT 3 builds (each returns the inserted row; we don't read back)
        .append_query_results([vec![make_build(
            BuildId::now_v7(),
            new_eval_id,
            BuildStatus::Substituted,
        )]])
        .append_query_results([vec![make_build(
            BuildId::now_v7(),
            new_eval_id,
            BuildStatus::Substituted,
        )]])
        .append_query_results([vec![make_build(
            BuildId::now_v7(),
            new_eval_id,
            BuildStatus::Substituted,
        )]])
        // 6. SELECT entry points: none
        .append_query_results([Vec::<gradient_entity::entry_point::Model>::new()])
        // 7. SELECT project for update read-back
        .append_query_results([vec![project.clone()]])
        // 8. UPDATE project
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result = trigger_restart_builds(&db, &project).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
    assert_eq!(
        result.unwrap().status,
        EvaluationStatus::Completed,
        "all-cached restart must start the new eval as Completed, not Building",
    );
}

/// When at least one previous build maps to `Queued`, the new eval must
/// start in `Building` so the dispatcher picks it up and the eventual
/// `check_evaluation_done` flips it to its terminal state.
#[tokio::test]
async fn restart_with_one_failed_inserts_building_eval() {
    let project = make_project();
    let prev_eval_id = EvaluationId::now_v7();
    let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Failed);
    let new_eval_id = EvaluationId::now_v7();

    let prev_builds = vec![
        make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Completed),
        make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::FailedPermanent),
    ];

    let inserted_eval = {
        let mut e = make_eval(new_eval_id, EvaluationStatus::Building);
        e.previous = Some(prev_eval_id);
        e
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<evaluation::Model>::new()])
        .append_query_results([vec![prev_eval]])
        .append_query_results([prev_builds])
        .append_query_results([vec![inserted_eval]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // find_active_leaders for the one Queued drv → no in-flight leader.
        //   same-org pass: empty
        //   cross-org pass: empty derivation lookup short-circuits
        .append_query_results([Vec::<gradient_entity::build::Model>::new()])
        .append_query_results([Vec::<gradient_entity::derivation::Model>::new()])
        .append_query_results([vec![make_build(
            BuildId::now_v7(),
            new_eval_id,
            BuildStatus::Substituted,
        )]])
        .append_query_results([vec![make_build(
            BuildId::now_v7(),
            new_eval_id,
            BuildStatus::Queued,
        )]])
        .append_query_results([Vec::<gradient_entity::entry_point::Model>::new()])
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result = trigger_restart_builds(&db, &project).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
    assert_eq!(result.unwrap().status, EvaluationStatus::Building);
}

/// Restarting must honour the cross-evaluation `via` dedup: if another
/// evaluation (typically a different project in the same organisation)
/// is currently building one of the drvs being restarted, the new build
/// row follows that leader instead of racing it. Regression for the
/// "rerun failed builds" path bypassing `find_active_leaders`.
#[tokio::test]
async fn restart_sets_via_when_leader_active_elsewhere() {
    let project = make_project();
    let prev_eval_id = EvaluationId::now_v7();
    let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Failed);
    let new_eval_id = EvaluationId::now_v7();

    let shared_drv = DerivationId::now_v7();
    let prev_build = make_build_drv(
        BuildId::now_v7(),
        prev_eval_id,
        shared_drv,
        BuildStatus::FailedPermanent,
    );

    // Leader currently Building under a different evaluation.
    let other_eval_id = EvaluationId::now_v7();
    let leader = make_build_drv(
        BuildId::now_v7(),
        other_eval_id,
        shared_drv,
        BuildStatus::Building,
    );
    let leader_id = leader.id;

    let inserted_eval = {
        let mut e = make_eval(new_eval_id, EvaluationStatus::Building);
        e.previous = Some(prev_eval_id);
        e
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<evaluation::Model>::new()])
        .append_query_results([vec![prev_eval]])
        .append_query_results([vec![prev_build]])
        .append_query_results([vec![inserted_eval]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // find_active_leaders for [shared_drv] → returns the in-flight leader.
        .append_query_results([vec![leader]])
        // INSERT new build (with via=leader_id).
        .append_query_results([vec![{
            let mut b = make_build_drv(
                BuildId::now_v7(),
                new_eval_id,
                shared_drv,
                BuildStatus::Queued,
            );
            b.via = Some(leader_id);
            b
        }]])
        .append_query_results([Vec::<gradient_entity::entry_point::Model>::new()])
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let result = trigger_restart_builds(&db, &project).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());

    // Verify the INSERT carried via=leader_id by inspecting the executed
    // statements. MockDatabase records every statement; the build insert
    // is the only one whose SQL mentions the `via` column.
    let logs = db.into_transaction_log();
    let build_insert = logs
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| {
            let sql = s.sql.to_lowercase();
            sql.contains("insert into") && sql.contains("\"build\"") && sql.contains("\"via\"")
        })
        .expect("expected an INSERT INTO build statement");
    let values: Vec<String> = build_insert
        .values
        .as_ref()
        .map(|v| v.0.iter().map(|val| format!("{:?}", val)).collect())
        .unwrap_or_default();
    let joined = values.join(", ");
    assert!(
        joined.contains(&leader_id.into_inner().to_string()),
        "expected build INSERT to carry via={} (leader id), got values: {}",
        leader_id,
        joined,
    );
}
