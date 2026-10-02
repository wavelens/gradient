/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use chrono::NaiveDateTime;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{DatabaseBackend, MockDatabase};

use crate::{Scheduler, loops, trigger_firing};

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

#[tokio::test]
async fn assign_queued_eval_enqueues_job() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();
    let project_id = ProjectId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        .append_query_results([no_membership()])
        .append_query_results([vec![make_commit(commit_id)]])
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        .append_query_results([vec![make_task(task_id, project_id)]])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "expected 1 job enqueued"
    );
}

#[tokio::test]
async fn assign_queued_eval_skips_already_enqueued() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();
    let project_id = ProjectId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        .append_query_results([no_membership()])
        .append_query_results([vec![make_commit(commit_id)]])
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        .append_query_results([vec![make_task(task_id, project_id)]])
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("first dispatch failed");
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("second dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        1,
        "second dispatch must be a no-op"
    );
}

#[tokio::test]
async fn assign_queued_eval_skips_missing_commit() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        .append_query_results([no_membership()])
        .append_query_results([Vec::<gradient_entity::commit::Model>::new()])
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        .append_query_results([Vec::<gradient_entity::task::Model>::new()])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "missing commit: no job should be enqueued"
    );
}

#[tokio::test]
async fn assign_queued_eval_without_task_is_skipped() {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, None)]])
        .append_query_results([no_membership()])
        .append_query_results([vec![make_commit(commit_id)]])
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("dispatch failed");

    assert_eq!(
        scheduler.pending_job_count().await,
        0,
        "eval without task must not be enqueued"
    );
}

/// The Queued select is carrying the open-row gate itself. A core with a rebuilt empty tracker or a
/// slow worker must not get the same evaluation twice.
#[tokio::test]
async fn assign_queued_evals_refuses_an_evaluation_whose_assignment_row_is_open() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<MEvaluation>::new()])
        .into_connection();
    let log_db = db.clone();

    let scheduler = make_scheduler(db).await;
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("dispatch");

    let log = log_db.into_transaction_log();
    let select = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| s.sql.starts_with("SELECT \"evaluation\""))
        .expect("the queued select ran");
    let gate = gradient_db::scheduling::assignment_record::no_open_assignment_predicate(
        &gradient_db::scheduling::assignment_record::eval_job_key_sql("\"evaluation\".\"id\""),
    );
    assert!(select.sql.contains(&gate), "{}", select.sql);
}

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

#[tokio::test]
async fn fire_once_skips_trigger_within_interval() {
    let task_id = TaskId::now_v7();
    let trigger_id = TaskTriggerId::now_v7();
    let project_id = ProjectId::now_v7();

    let recent = gradient_types::now();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![make_polling_trigger(
            trigger_id,
            task_id,
            60,
            Some(recent),
        )]])
        .append_query_results([vec![make_task(task_id, project_id)]])
        .into_connection();

    let scheduler = make_scheduler(db).await;
    trigger_firing::fire_once(&scheduler)
        .await
        .expect("dispatch_once should not fail");
    assert_eq!(scheduler.pending_job_count().await, 0);
}

fn no_membership() -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
    Vec::new()
}

fn membership_of(
    evaluation: EvaluationId,
    status: gradient_entity::cluster_job::ClusterJobStatus,
    count: i64,
) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
    let now = test_date();
    std::collections::BTreeMap::from([
        ("member_id", ClusterMemberId::now_v7().into_inner().into()),
        ("cluster_job", ClusterJobId::now_v7().into_inner().into()),
        ("evaluation", Some(evaluation.into_inner()).into()),
        ("derivation_build", Option::<uuid::Uuid>::None.into()),
        ("role", "node".into()),
        ("primary", false.into()),
        ("pin", Option::<String>::None.into()),
        ("status", i16::from(status).into()),
        ("same_zone", false.into()),
        ("attempts", 0i32.into()),
        ("retry_budget", 1i32.into()),
        ("created_at", now.into()),
        ("updated_at", now.into()),
        ("member_count", count.into()),
    ])
}

async fn assign_one_member(
    status: gradient_entity::cluster_job::ClusterJobStatus,
    count: i64,
    eval_only_worker: bool,
) -> (Arc<Scheduler>, EvaluationId) {
    let eval_id = EvaluationId::now_v7();
    let commit_id = CommitId::now_v7();
    let task_id = TaskId::now_v7();
    let mut db = MockDatabase::new(DatabaseBackend::Postgres);
    if eval_only_worker {
        db = db.append_query_results([no_membership()]);
    }
    let db = db
        .append_query_results([vec![make_eval_queued(eval_id, commit_id, Some(task_id))]])
        .append_query_results([vec![membership_of(eval_id, status, count)]])
        .append_query_results([vec![make_commit(commit_id)]])
        .append_query_results([
            Vec::<gradient_entity::evaluation_flake_input_override::Model>::new(),
        ])
        .append_query_results([vec![make_task(task_id, ProjectId::now_v7())]])
        .into_connection();
    let scheduler = make_scheduler(db).await;
    if eval_only_worker {
        scheduler
            .register_worker(
                "w1",
                crate::scheduler_tests::eval_worker_caps(),
                std::collections::HashSet::new(),
                crate::scheduler_tests::port().0,
            )
            .await
            .expect("register");
    }
    loops::assign_queued_evals(&scheduler)
        .await
        .expect("dispatch");
    (scheduler, eval_id)
}

#[tokio::test]
async fn a_cluster_member_evaluation_waits_for_its_cluster() {
    let (scheduler, eval_id) = assign_one_member(
        gradient_entity::cluster_job::ClusterJobStatus::Queued,
        2,
        false,
    )
    .await;

    assert_eq!(scheduler.pending_job_count().await, 0);
    let key = crate::jobs::eval_job_key(eval_id);
    assert!(scheduler.untracked(vec![key]).await.is_empty());
}

/// A split fetch-only job would hand its evaluation to a follow-up outside the cluster. A cluster
/// member is therefore always evaluating in one job.
#[tokio::test]
async fn a_cluster_member_evaluation_is_never_split() {
    let (scheduler, _) = assign_one_member(
        gradient_entity::cluster_job::ClusterJobStatus::Queued,
        1,
        true,
    )
    .await;

    let snapshot = scheduler.cluster_snapshot().await;
    let job = snapshot.clusters[0].members[0]
        .job
        .as_ref()
        .expect("member job");
    let crate::jobs::PendingJob::Eval(eval) = job else {
        panic!("expected an eval member");
    };
    assert!(
        eval.job
            .steps
            .contains(&gradient_wire::types::FlakeStep::EvaluateFlake)
    );
}

#[tokio::test]
async fn a_member_of_a_running_cluster_is_held_back() {
    let (scheduler, eval_id) = assign_one_member(
        gradient_entity::cluster_job::ClusterJobStatus::Running,
        2,
        false,
    )
    .await;

    assert_eq!(scheduler.pending_job_count().await, 0);
    let key = crate::jobs::eval_job_key(eval_id);
    assert_eq!(scheduler.untracked(vec![key.clone()]).await, vec![key]);
}
