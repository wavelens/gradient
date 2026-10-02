/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::{ApplyInput, ApplyOutcome, ApprovalInfo, apply_trigger, park_if_storage_full};
use fixtures::{
    input, make_commit, make_eval, make_task_with_concurrency, make_task_with_last_eval,
    with_eval_worker, with_storage_not_full, with_writable_cache,
};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

#[tokio::test]
async fn storage_gate_ignores_non_queued_eval() {
    let already_waiting = make_eval(
        EvaluationId::now_v7(),
        TaskId::nil(),
        CommitId::now_v7(),
        EvaluationStatus::Waiting,
    );
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let out = park_if_storage_full(&db, already_waiting.clone(), ProjectId::nil(), 0)
        .await
        .unwrap();
    assert_eq!(out.status, EvaluationStatus::Waiting);
    assert_eq!(out.id, already_waiting.id);
}

#[tokio::test]
async fn skips_when_same_commit_as_last_eval() {
    let prev_eval_id = EvaluationId::now_v7();
    let prev_commit_id = CommitId::now_v7();
    let task = make_task_with_last_eval(Some(prev_eval_id));
    let same_hash = vec![1u8; 20];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_eval(
            prev_eval_id,
            task.id,
            prev_commit_id,
            EvaluationStatus::Completed,
        )]])
        .append_query_results([vec![make_commit(prev_commit_id, same_hash.clone())]])
        .into_connection();

    let trig = TaskTriggerId::now_v7();
    let res = apply_trigger(
        &db,
        &task,
        input(trig, TriggerType::Polling, same_hash, false),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::SkippedSameCommit));
}

#[tokio::test]
async fn time_trigger_bypasses_same_commit_check() {
    let prev_eval_id = EvaluationId::now_v7();
    let task = make_task_with_last_eval(Some(prev_eval_id));
    let same_hash = vec![1u8; 20];
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_eval(
            prev_eval_id,
            task.id,
            CommitId::nil(),
            EvaluationStatus::Completed,
        )]])
        .append_query_results([vec![make_commit(new_commit_id, same_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(
                new_eval_id,
                task.id,
                new_commit_id,
                EvaluationStatus::Queued,
            );
            m.trigger = Some(trig);
            m
        }]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(&db, &task, input(trig, TriggerType::Time, same_hash, false))
        .await
        .unwrap();
    assert!(matches!(res, ApplyOutcome::Created { .. }));
}

#[tokio::test]
async fn skip_concurrency_with_running_eval() {
    let task = make_task_with_last_eval(None);
    let running_eval_id = EvaluationId::now_v7();
    let running_eval = make_eval(
        running_eval_id,
        task.id,
        CommitId::nil(),
        EvaluationStatus::Building,
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![running_eval.clone()]])
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        .into_connection();

    let trig = TaskTriggerId::now_v7();
    let res = apply_trigger(
        &db,
        &task,
        input(trig, TriggerType::Polling, vec![9u8; 20], false),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::SkippedConcurrency));
}

#[tokio::test]
async fn polling_with_in_flight_same_commit_skips_without_aborting() {
    // A poll seeing the commit being built must not abort the running evaluation. Dedup against
    // the in-flight evaluation's commit must catch it ahead of the concurrency policy, even with a
    // dangling `last_evaluation`.
    let task = make_task_with_concurrency(None, ConcurrencyPolicy::SoftAbort);
    let running_eval_id = EvaluationId::now_v7();
    let running_commit_id = CommitId::now_v7();
    let same_hash = vec![3u8; 20];
    let running_eval = make_eval(
        running_eval_id,
        task.id,
        running_commit_id,
        EvaluationStatus::Building,
    );

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![running_eval.clone()]])
        .append_query_results([vec![make_commit(running_commit_id, same_hash.clone())]])
        .into_connection();

    let trig = TaskTriggerId::now_v7();
    let res = apply_trigger(
        &db,
        &task,
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
    let task = make_task_with_concurrency(None, ConcurrencyPolicy::All);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();
    let new_hash = vec![9u8; 20];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(
                new_eval_id,
                task.id,
                new_commit_id,
                EvaluationStatus::Queued,
            );
            m.trigger = Some(trig);
            m.concurrent = true;
            m
        }]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &task,
        input(trig, TriggerType::Polling, new_hash, false),
    )
    .await
    .unwrap();

    let ApplyOutcome::Created {
        evaluation,
        aborted_evaluation,
        hard_abort,
    } = res
    else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.id, new_eval_id);
    assert!(evaluation.concurrent, "new eval must carry concurrent=true");
    assert_eq!(aborted_evaluation, None);
    assert!(!hard_abort);
}

#[tokio::test]
async fn unique_constraint_violation_returns_skipped_concurrency() {
    let task = make_task_with_last_eval(None);
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, vec![1u8; 20])]])
        .append_query_errors([sea_orm::DbErr::Custom(
            "uq_evaluation_one_active_per_task".into(),
        )])
        .into_connection();

    let res = apply_trigger(
        &db,
        &task,
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
    let task = make_task_with_last_eval(Some(prev_eval_id));
    let same_hash = vec![1u8; 20];
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_eval(
            prev_eval_id,
            task.id,
            CommitId::nil(),
            EvaluationStatus::Completed,
        )]])
        .append_query_results([vec![make_commit(new_commit_id, same_hash.clone())]])
        .append_query_results([vec![make_eval(
            new_eval_id,
            task.id,
            new_commit_id,
            EvaluationStatus::Queued,
        )]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &task,
        input(trig, TriggerType::Polling, same_hash, true),
    )
    .await
    .unwrap();
    assert!(matches!(res, ApplyOutcome::Created { .. }));
}

#[tokio::test]
async fn hard_abort_populates_aborted_fields() {
    let task = make_task_with_concurrency(None, ConcurrencyPolicy::HardAbort);
    let running_eval_id = EvaluationId::now_v7();
    let running_eval = make_eval(
        running_eval_id,
        task.id,
        CommitId::nil(),
        EvaluationStatus::Building,
    );
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();
    let new_hash = vec![7u8; 20];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![running_eval.clone()]])
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        .append_query_results([vec![running_eval.clone()]])
        .append_query_results([vec![running_eval.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(
                new_eval_id,
                task.id,
                new_commit_id,
                EvaluationStatus::Queued,
            );
            m.trigger = Some(trig);
            m
        }]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_eval_worker(with_storage_not_full(with_writable_cache(db))).into_connection();

    let res = apply_trigger(
        &db,
        &task,
        input(trig, TriggerType::Polling, new_hash, false),
    )
    .await
    .unwrap();

    let ApplyOutcome::Created {
        evaluation,
        aborted_evaluation,
        hard_abort,
    } = res
    else {
        panic!("expected Created, got {res:?}");
    };
    assert_eq!(evaluation.id, new_eval_id);
    assert_eq!(aborted_evaluation, Some(running_eval_id));
    assert!(hard_abort, "the caller must abort the eval's shared builds");
}

#[tokio::test]
async fn gate_approval_parks_pr_evaluation_in_waiting_approval() {
    use gradient_types::waiting_reason::WaitingReason;
    let task = make_task_with_last_eval(None);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();
    let new_hash = vec![1u8; 20];

    let parked_eval = {
        let mut m = make_eval(
            new_eval_id,
            task.id,
            new_commit_id,
            EvaluationStatus::Waiting,
        );
        m.trigger = Some(trig);
        m.waiting_reason = Some(WaitingReason::approval(42, "external-contrib").to_json());
        m
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(
                new_eval_id,
                task.id,
                new_commit_id,
                EvaluationStatus::Queued,
            );
            m.trigger = Some(trig);
            m
        }]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
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
    let res = apply_trigger(&db, &task, applied).await.unwrap();

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

#[tokio::test]
async fn no_writable_cache_parks_evaluation_in_waiting_no_cache() {
    use gradient_types::waiting_reason::WaitingReason;
    let task = make_task_with_last_eval(None);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();
    let new_hash = vec![1u8; 20];

    let parked_eval = {
        let mut m = make_eval(
            new_eval_id,
            task.id,
            new_commit_id,
            EvaluationStatus::Waiting,
        );
        m.trigger = Some(trig);
        m.waiting_reason = Some(WaitingReason::NoCache.to_json());
        m
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(
                new_eval_id,
                task.id,
                new_commit_id,
                EvaluationStatus::Queued,
            );
            m.trigger = Some(trig);
            m
        }]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
        .append_query_results([vec![parked_eval.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let res = apply_trigger(
        &db,
        &task,
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

/// The build-dispatch repair pass is stalling `Queued` evaluations only when zero workers are
/// connected. Connected workers without `eval` would leave the evaluation `Queued` forever without
/// this gate.
#[tokio::test]
async fn no_eval_capable_worker_parks_evaluation_in_waiting_workers() {
    use gradient_types::waiting_reason::WaitingReason;
    let task = make_task_with_last_eval(None);
    let new_eval_id = EvaluationId::now_v7();
    let new_commit_id = CommitId::now_v7();
    let trig = TaskTriggerId::now_v7();
    let new_hash = vec![1u8; 20];

    let parked_eval = {
        let mut m = make_eval(
            new_eval_id,
            task.id,
            new_commit_id,
            EvaluationStatus::Waiting,
        );
        m.trigger = Some(trig);
        m.waiting_reason = Some(WaitingReason::workers(Vec::new(), 0, Vec::new()).to_json());
        m
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()])
        .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
        .append_query_results([vec![{
            let mut m = make_eval(
                new_eval_id,
                task.id,
                new_commit_id,
                EvaluationStatus::Queued,
            );
            m.trigger = Some(trig);
            m
        }]])
        .append_query_results([Vec::<gradient_entity::task_flake_input_override::Model>::new()])
        .append_query_results([vec![task.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let db = with_writable_cache(db);
    let db = with_storage_not_full(db)
        .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
        .append_query_results([Vec::<gradient_entity::project_base_worker::Model>::new()])
        .append_query_results([vec![parked_eval.clone()]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    let res = apply_trigger(
        &db,
        &task,
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
