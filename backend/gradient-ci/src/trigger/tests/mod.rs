/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::*;
use gradient_types::*;
use fixtures::{make_anchor, make_entry_point, make_eval, make_project};
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

/// Regression for the "evaluations stuck in Building forever" symptom: when
/// every entry-point anchor is already terminal-success there is nothing to
/// rebuild, so the new evaluation must start in `Completed` rather than
/// `Building`, otherwise nothing fires `check_evaluation_done` and the row is
/// stuck.
#[tokio::test]
async fn restart_with_all_cached_inserts_completed_eval() {
    let project = make_project();
    let prev_eval_id = EvaluationId::now_v7();
    let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Completed);
    let new_eval_id = EvaluationId::now_v7();

    let drv_a = DerivationId::now_v7();
    let drv_b = DerivationId::now_v7();
    let prev_entry_points = vec![
        make_entry_point(prev_eval_id, drv_a),
        make_entry_point(prev_eval_id, drv_b),
    ];
    let anchors = vec![
        make_anchor(drv_a, BuildStatus::Completed),
        make_anchor(drv_b, BuildStatus::Substituted),
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
        // 3. load prev entry points
        .append_query_results([prev_entry_points])
        // 4. load anchors for the entry-point derivations (all terminal-success)
        .append_query_results([anchors])
        // 5. INSERT new evaluation: returns the row with status=Completed
        .append_query_results([vec![inserted_eval]])
        // 6. snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // 7. copy entry points: two INSERTs
        .append_query_results([vec![make_entry_point(new_eval_id, drv_a)]])
        .append_query_results([vec![make_entry_point(new_eval_id, drv_b)]])
        // 8. SELECT project for update read-back
        .append_query_results([vec![project.clone()]])
        // 9. UPDATE project
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

/// When at least one entry-point anchor is not terminal-success, the new eval
/// must start in `Building` so the dispatcher re-resolves and re-runs it.
#[tokio::test]
async fn restart_with_one_failed_inserts_building_eval() {
    let project = make_project();
    let prev_eval_id = EvaluationId::now_v7();
    let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Failed);
    let new_eval_id = EvaluationId::now_v7();

    let drv_a = DerivationId::now_v7();
    let drv_b = DerivationId::now_v7();
    let prev_entry_points = vec![
        make_entry_point(prev_eval_id, drv_a),
        make_entry_point(prev_eval_id, drv_b),
    ];
    let anchors = vec![
        make_anchor(drv_a, BuildStatus::Completed),
        make_anchor(drv_b, BuildStatus::FailedPermanent),
    ];

    let inserted_eval = {
        let mut e = make_eval(new_eval_id, EvaluationStatus::Building);
        e.previous = Some(prev_eval_id);
        e
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<evaluation::Model>::new()])
        .append_query_results([vec![prev_eval]])
        .append_query_results([prev_entry_points])
        .append_query_results([anchors])
        .append_query_results([vec![inserted_eval]])
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        .append_query_results([vec![make_entry_point(new_eval_id, drv_a)]])
        .append_query_results([vec![make_entry_point(new_eval_id, drv_b)]])
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

fn open_pr_action(project_id: ProjectId) -> MProjectAction {
    MProjectAction {
        id: ProjectActionId::now_v7(),
        project: project_id,
        name: "updater".into(),
        action_type: ActionType::OpenPr.to_i16(),
        config: serde_json::to_value(ActionConfig::OpenPr {
            integration_id: IntegrationId::now_v7(),
            generator: PatchGeneratorKind::FlakeLock,
            granularity: PrGranularity::PerRun,
            verify_gate: VerifyGate::Build,
            branch_pattern: "gradient/flake-lock-update".into(),
            title_template: None,
            body_template: None,
            update_existing: true,
        })
        .unwrap(),
        events: serde_json::json!([]),
        active: true,
        ..Default::default()
    }
}

fn tracked_override(
    project_id: ProjectId,
    name: &str,
    url: Option<String>,
) -> MProjectFlakeInputOverride {
    MProjectFlakeInputOverride {
        id: FlakeInputOverrideId::now_v7(),
        project: project_id,
        input_name: name.into(),
        url,
        ..Default::default()
    }
}

#[tokio::test]
async fn input_update_noop_without_open_pr_action() {
    let project = make_project();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::project_action::Model>::new()])
        .into_connection();

    let created = maybe_trigger_input_update(&db, &project, vec![0u8; 20], None, None, None)
        .await
        .unwrap();
    assert!(created.is_empty());
}

#[tokio::test]
async fn input_update_noop_without_tracked_inputs() {
    let project = make_project();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![open_pr_action(project.id)]])
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        .into_connection();

    let created = maybe_trigger_input_update(&db, &project, vec![0u8; 20], None, None, None)
        .await
        .unwrap();
    assert!(created.is_empty());
}

#[tokio::test]
async fn input_update_pinned_override_blocks_run() {
    let project = make_project();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![open_pr_action(project.id)]])
        .append_query_results([vec![tracked_override(
            project.id,
            "nixpkgs",
            Some("github:NixOS/nixpkgs".into()),
        )]])
        .into_connection();

    let created = maybe_trigger_input_update(&db, &project, vec![0u8; 20], None, None, None)
        .await
        .unwrap();
    assert!(created.is_empty());
}

#[tokio::test]
async fn input_update_creates_eval_for_tracked_input() {
    let project = make_project();
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![open_pr_action(project.id)]])
        .append_query_results([vec![tracked_override(project.id, "nixpkgs", None)]])
        .append_query_results([Vec::<evaluation::Model>::new()])
        .append_query_results([vec![gradient_entity::commit::Model {
            id: commit_id,
            hash: vec![0u8; 20],
            ..Default::default()
        }]])
        .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Queued)]])
        .append_query_results([vec![MEvaluationInputUpdate {
            id: EvaluationInputUpdateId::now_v7(),
            evaluation: eval_id,
            ..Default::default()
        }]])
        .into_connection();

    let created = maybe_trigger_input_update(
        &db,
        &project,
        vec![0u8; 20],
        Some("msg".into()),
        Some("author".into()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(created, vec![eval_id]);
}
