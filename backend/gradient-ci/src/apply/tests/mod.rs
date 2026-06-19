/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::{ApplyInput, ApplyOutcome, ApprovalInfo, apply_trigger, park_if_storage_full};
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use fixtures::{
    input, make_commit, make_eval, make_project_with_concurrency, make_project_with_last_eval,
    with_eval_worker, with_storage_not_full, with_writable_cache,
};
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

/// The storage gate only acts on `Queued` evaluations; a row already
/// parked (e.g. by the approval gate) is returned untouched without
/// issuing any cache queries.
#[tokio::test]
async fn storage_gate_ignores_non_queued_eval() {
    let already_waiting = make_eval(
        EvaluationId::now_v7(),
        ProjectId::nil(),
        CommitId::now_v7(),
        EvaluationStatus::Waiting,
    );
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let out = park_if_storage_full(&db, already_waiting.clone(), OrganizationId::nil(), 0)
        .await
        .unwrap();
    assert_eq!(out.status, EvaluationStatus::Waiting);
    assert_eq!(out.id, already_waiting.id);
}

#[tokio::test]
async fn skips_when_same_commit_as_last_eval() {
    let prev_eval_id = EvaluationId::now_v7();
    let prev_commit_id = CommitId::now_v7();
    let project = make_project_with_last_eval(Some(prev_eval_id));
    let same_hash = vec![1u8; 20];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Same-commit dedup: fetch prev eval
        .append_query_results([vec![make_eval(
            prev_eval_id,
            project.id,
            prev_commit_id,
            EvaluationStatus::Completed,
        )]])
        // Same-commit dedup: fetch prev commit
        .append_query_results([vec![make_commit(prev_commit_id, same_hash.clone())]])
        .into_connection();

    let trig = ProjectTriggerId::now_v7();
    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, same_hash, false),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::SkippedSameCommit));
}

#[tokio::test]
async fn time_trigger_bypasses_same_commit_check() {
    let prev_eval_id = EvaluationId::now_v7();
    let project = make_project_with_last_eval(Some(prev_eval_id));
    let same_hash = vec![1u8; 20];
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // No same-commit dedup queries (time bypasses)
        // Concurrency check: no in-flight
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation internal in-progress check (none)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: resolve previous (returns the prev eval row)
        .append_query_results([vec![make_eval(
            prev_eval_id,
            project.id,
            CommitId::nil(),
            EvaluationStatus::Completed,
        )]])
        // commit insert
        .append_query_results([vec![make_commit(new_commit_id, same_hash.clone())]])
        // evaluation insert
        .append_query_results([vec![{
            let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m
        }]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // project update read-back
        .append_query_results([vec![project.clone()]])
        // project update exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Time, same_hash, false),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::Created { .. }));
}

#[tokio::test]
async fn skip_concurrency_with_running_eval() {
    let project = make_project_with_last_eval(None);
    let running_eval_id = EvaluationId::now_v7();
    let running_eval = make_eval(
        running_eval_id,
        project.id,
        CommitId::nil(),
        EvaluationStatus::Building,
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // in_flight lookup returns the running eval
        .append_query_results([vec![running_eval.clone()]])
        // dedup against running's commit: row missing → fall through
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        // No last_evaluation, so no further dedup queries.
        // Concurrency policy reuses the in-flight eval - Skip => SkippedConcurrency.
        .into_connection();

    let trig = ProjectTriggerId::now_v7();
    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, vec![9u8; 20], false),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::SkippedConcurrency));
}

#[tokio::test]
async fn polling_with_in_flight_same_commit_skips_without_aborting() {
    // Regression: a polling trigger that observes the same commit currently
    // being built must NOT abort the running evaluation. Even if
    // last_evaluation is dangling or missing, dedup against the in-flight
    // eval's commit catches it before the concurrency policy fires.
    let project = make_project_with_concurrency(None, 1); // SoftAbort
    let running_eval_id = EvaluationId::now_v7();
    let running_commit_id = CommitId::now_v7();
    let same_hash = vec![3u8; 20];
    let running_eval = make_eval(
        running_eval_id,
        project.id,
        running_commit_id,
        EvaluationStatus::Building,
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // in_flight lookup returns the running eval
        .append_query_results([vec![running_eval.clone()]])
        // dedup fetches the running eval's commit - same hash as the poll
        .append_query_results([vec![make_commit(running_commit_id, same_hash.clone())]])
        // dedup short-circuits with SkippedSameCommit; no abort, no insert
        .into_connection();

    let trig = ProjectTriggerId::now_v7();
    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, same_hash, false),
    )
    .await
    .unwrap();
    assert!(
        matches!(res, ApplyOutcome::SkippedSameCommit),
        "expected SkippedSameCommit, got {res:?}"
    );
}

#[tokio::test]
async fn all_concurrency_creates_evaluation_alongside_running() {
    let project = make_project_with_concurrency(None, 2); // All
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();
    let new_hash = vec![9u8; 20];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // in_flight lookup runs unconditionally - return empty for this test
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // all policy skips the in-flight concurrency action
        // trigger_evaluation: concurrent=true skips the in-progress guard - no guard query
        // trigger_evaluation: resolve previous (no last_evaluation)
        // commit insert
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        // evaluation insert - the new eval carries concurrent=true
        .append_query_results([vec![{
            let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m.concurrent = true;
            m
        }]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // project update read-back
        .append_query_results([vec![project.clone()]])
        // project update exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, new_hash, false),
    )
    .await
    .unwrap();

    let ApplyOutcome::Created {
        evaluation,
        aborted_evaluation,
        aborted_anchors,
    } = res
    else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.id, new_eval_id);
    assert!(evaluation.concurrent, "new eval must carry concurrent=true");
    assert_eq!(aborted_evaluation, None);
    assert!(aborted_anchors.is_empty());
}

#[tokio::test]
async fn unique_constraint_violation_returns_skipped_concurrency() {
    let project = make_project_with_last_eval(None);
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Concurrency check: no in-flight (races past the guard)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: no in-progress guard
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // commit insert
        .append_query_results([vec![make_commit(new_commit_id, vec![1u8; 20])]])
        // evaluation insert fails with unique constraint violation
        .append_query_errors([sea_orm::DbErr::Custom(
            "uq_evaluation_one_active_per_project".into(),
        )])
        .into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, vec![1u8; 20], false),
    )
    .await
    .unwrap();
    assert!(
        matches!(res, ApplyOutcome::SkippedConcurrency),
        "expected SkippedConcurrency, got {res:?}"
    );
}

#[tokio::test]
async fn manual_bypasses_same_commit_check() {
    let prev_eval_id = EvaluationId::now_v7();
    let project = make_project_with_last_eval(Some(prev_eval_id));
    let same_hash = vec![1u8; 20];
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // manual=true skips same-commit dedup entirely
        // Concurrency check: no in-flight
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation internal in-progress check
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: resolve previous (prev row exists)
        .append_query_results([vec![make_eval(
            prev_eval_id,
            project.id,
            CommitId::nil(),
            EvaluationStatus::Completed,
        )]])
        // commit insert
        .append_query_results([vec![make_commit(new_commit_id, same_hash.clone())]])
        // evaluation insert
        .append_query_results([vec![make_eval(
            new_eval_id,
            project.id,
            new_commit_id,
            EvaluationStatus::Queued,
        )]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // project update read-back
        .append_query_results([vec![project.clone()]])
        // project update exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, same_hash, true),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::Created { .. }));
}

#[tokio::test]
async fn hard_abort_populates_aborted_fields() {
    let project = make_project_with_concurrency(None, 0); // HardAbort
    let running_eval_id = EvaluationId::now_v7();
    let running_eval = make_eval(
        running_eval_id,
        project.id,
        CommitId::nil(),
        EvaluationStatus::Building,
    );
    let active_anchor_id = DerivationBuildId::now_v7();
    let active_anchor = gradient_entity::derivation_build::Model {
        id: active_anchor_id,
        derivation: DerivationId::nil(),
        status: gradient_entity::build::BuildStatus::Building,
        ..Default::default()
    };
    let active_job = gradient_entity::build_job::Model {
        id: BuildJobId::now_v7(),
        evaluation: running_eval_id,
        derivation: DerivationId::nil(),
        derivation_build: active_anchor_id,
        ..Default::default()
    };
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();
    let new_hash = vec![7u8; 20];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // in_flight lookup: returns the running eval
        .append_query_results([vec![running_eval.clone()]])
        // dedup fetches the running eval's commit - row missing → fall through
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        // abort_evaluation: eval fetch
        .append_query_results([vec![running_eval.clone()]])
        // abort_evaluation: eval update read-back
        .append_query_results([vec![running_eval.clone()]])
        // abort_evaluation: eval exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // abort_evaluation: build_job anchor ids for the eval
        .append_query_results([vec![active_job.clone()]])
        // abort_evaluation: active anchors (Building)
        .append_query_results([vec![active_anchor.clone()]])
        // abort_evaluation: shared lookup - no other eval references the anchor
        .append_query_results([Vec::<gradient_entity::build_job::Model>::new()])
        // abort_evaluation: anchor refetch for update
        .append_query_results([vec![active_anchor.clone()]])
        // abort_evaluation: anchor exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // trigger_evaluation: in-progress guard
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: commit insert
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        // trigger_evaluation: eval insert
        .append_query_results([vec![{
            let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m
        }]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // trigger_evaluation: project update read-back
        .append_query_results([vec![project.clone()]])
        // trigger_evaluation: project exec
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, new_hash, false),
    )
    .await
    .unwrap();

    let ApplyOutcome::Created {
        evaluation,
        aborted_evaluation,
        aborted_anchors,
    } = res
    else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.id, new_eval_id);
    assert_eq!(aborted_evaluation, Some(running_eval_id));
    assert_eq!(aborted_anchors, vec![active_anchor_id]);
}

/// When the PR webhook layer flags a freshly-created evaluation as needing
/// maintainer approval, `apply_trigger` parks it in `Waiting + Approval`
/// before checking the cache gate. The NoCache check then early-returns
/// because the eval is no longer in `Queued` status.
#[tokio::test]
async fn gate_approval_parks_pr_evaluation_in_waiting_approval() {
    use gradient_types::waiting_reason::WaitingReason;
    let project = make_project_with_last_eval(None);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();
    let new_hash = vec![1u8; 20];

    let parked_eval = {
        let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Waiting);
        m.trigger = Some(trig);
        m.waiting_reason = Some(WaitingReason::approval(42, "external-contrib").to_json());
        m
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m
        }]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // park_if_pending_approval: update returns the parked row
        .append_query_results([vec![parked_eval.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let applied = ApplyInput {
        trigger_id: trig,
        trigger_type: TriggerType::ReporterPullRequest,
        commit_hash: new_hash,
        commit_message: None,
        author_name: None,
        manual: false,
        gate_approval: Some(ApprovalInfo {
            pr_number: 42,
            pr_author: "external-contrib".into(),
        }),
        repository_override: None,
        wildcard_override: None,
        source_comment: None,
        instance_max_storage_gb: 0,
    };
    let res = apply_trigger(&db, &project, applied).await.unwrap();

    let ApplyOutcome::Created { evaluation, .. } = res else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.status, EvaluationStatus::Waiting);
    let reason = evaluation
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
        .expect("waiting_reason must be set");
    match reason {
        WaitingReason::Approval {
            pr_number,
            pr_author,
        } => {
            assert_eq!(pr_number, 42);
            assert_eq!(pr_author, "external-contrib");
        }
        other => panic!("expected Approval, got {other:?}"),
    }
}

/// When the project's organisation has no writable cache subscription, a
/// freshly-created evaluation is parked in `Waiting` with the `NoCache`
/// reason - no jobs are spawned and the scheduler's reconciler must leave
/// the row alone until the cache-create endpoint re-queues it.
#[tokio::test]
async fn no_writable_cache_parks_evaluation_in_waiting_no_cache() {
    use gradient_types::waiting_reason::WaitingReason;
    let project = make_project_with_last_eval(None);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();
    let new_hash = vec![1u8; 20];

    let parked_eval = {
        let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Waiting);
        m.trigger = Some(trig);
        m.waiting_reason = Some(WaitingReason::NoCache.to_json());
        m
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Concurrency check: no in-flight
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: in-progress guard
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: commit insert
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        // trigger_evaluation: eval insert (initially Queued)
        .append_query_results([vec![{
            let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m
        }]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // trigger_evaluation: project update read-back + exec
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // org_has_writable_cache: no organization_cache rows → returns false
        .append_query_results([Vec::<gradient_entity::organization_cache::Model>::new()])
        // Park: update eval read-back + exec, returns the parked row
        .append_query_results([vec![parked_eval.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, new_hash, false),
    )
    .await
    .unwrap();

    let ApplyOutcome::Created { evaluation, .. } = res else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.status, EvaluationStatus::Waiting);
    let reason = evaluation
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
        .expect("waiting_reason must be set");
    assert!(matches!(reason, WaitingReason::NoCache));
}

/// When the project's organisation has a writable cache but no active
/// worker registration with `enable_eval`, `apply_trigger` parks the
/// freshly-created evaluation in `Waiting + Workers { connected_workers: 0 }`.
/// Without this gate the eval would sit `Queued` forever - the
/// build-dispatch reconciler only stalls Queued evals when zero workers
/// are connected, not when connected workers lack `eval`.
#[tokio::test]
async fn no_eval_capable_worker_parks_evaluation_in_waiting_workers() {
    use gradient_types::waiting_reason::WaitingReason;
    let project = make_project_with_last_eval(None);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = ProjectTriggerId::now_v7();
    let new_hash = vec![1u8; 20];

    let parked_eval = {
        let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Waiting);
        m.trigger = Some(trig);
        m.waiting_reason = Some(WaitingReason::workers(Vec::new(), 0, Vec::new()).to_json());
        m
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // Concurrency check: no in-flight
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: in-progress guard
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        // trigger_evaluation: commit insert
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        // trigger_evaluation: eval insert (initially Queued)
        .append_query_results([vec![{
            let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
            m.trigger = Some(trig);
            m
        }]])
        // snapshot flake input overrides (none)
        .append_query_results([Vec::<gradient_entity::project_flake_input_override::Model>::new()])
        // trigger_evaluation: project update read-back + exec
        .append_query_results([vec![project.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    // park_if_no_cache: writable cache exists → returns unchanged.
    let db = with_writable_cache(db);
    // park_if_storage_full: no writable cache rows → not full.
    let db = with_storage_not_full(db)
        // park_if_no_workers: no eval-capable registration and no base worker
        // enabled for this org → park.
        .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
        .append_query_results([Vec::<gradient_entity::organization_base_worker::Model>::new()])
        // Park: update eval read-back + exec, returns the parked row.
        .append_query_results([vec![parked_eval.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let res = apply_trigger(
        &db,
        &project,
        input(trig, TriggerType::Polling, new_hash, false),
    )
    .await
    .unwrap();

    let ApplyOutcome::Created { evaluation, .. } = res else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.status, EvaluationStatus::Waiting);
    let reason = evaluation
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
        .expect("waiting_reason must be set");
    match reason {
        WaitingReason::Workers {
            connected_workers,
            unmet,
            available_architectures,
        } => {
            assert_eq!(connected_workers, 0);
            assert!(unmet.is_empty());
            assert!(available_architectures.is_empty());
        }
        other => panic!("expected Workers, got {other:?}"),
    }
}
