/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::deliveries::trigger_fire::TriggerFire;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{self, EvaluationKind, EvaluationStatus};
use gradient_entity::ids::{CacheId, EvaluationId, TaskTriggerId};
use gradient_entity::pending_delivery::{self, PendingDeliveryKind};
use gradient_entity::task::ConcurrencyPolicy;
use gradient_entity::task_trigger::TriggerType;
use gradient_entity::{commit, task};
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_wire::types::JobKind;

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, CaseShapes, GitRepo, SchedulerCase, ago, attr_of, drv_path,
    eval_key, is_recent, polling, time_trigger, w_all,
};

const W1: &str = "w1";
const W2: &str = "w2";
const FAR: u64 = 5_000_000;

#[tokio::test]
async fn t01_a_due_polling_trigger_gets_a_single_undelivered_firing() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(due_polling(e0, t1, &repo)).await;
    let evaluations = case.evaluations().await;

    case.fire_once().await;
    let deliveries = case.deliveries().await;
    let [row] = deliveries.as_slice() else {
        panic!("after 1: a single pending_delivery row, got {deliveries:?}");
    };
    let firing = TriggerFire {
        trigger: t1,
        last_fired_at: None,
    };
    assert_eq!(
        (
            row.kind,
            row.key.clone(),
            row.payload.clone(),
            row.attempts,
            row.delivered_at,
        ),
        (
            PendingDeliveryKind::TriggerFire,
            firing.key(),
            serde_json::to_value(firing).expect("a trigger fire payload"),
            0,
            None,
        ),
        "after 1: pending_delivery"
    );
    assert!(
        is_recent(row.next_attempt_at),
        "after 1: next_attempt_at ~now"
    );
    assert_eq!(
        case.trigger(t1).await.last_fired_at,
        None,
        "after 1: task_trigger(T1).last_fired_at"
    );
    assert_eq!(
        case.evaluations().await,
        evaluations,
        "after 1: evaluation rows unchanged"
    );
    case.check("step 1").await;

    case.fire_once().await;
    assert_eq!(
        case.deliveries().await.len(),
        1,
        "after 2: pending_delivery rows"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn t02_two_instances_firing_a_due_trigger_leave_a_single_delivery() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(due_polling(e0, t1, &repo)).await;
    let second = case.second_instance().await;

    case.fire_once().await;
    second.fire_once().await;
    let key = TriggerFire {
        trigger: t1,
        last_fired_at: None,
    }
    .key();
    let firings: Vec<_> = case
        .deliveries()
        .await
        .into_iter()
        .filter(|d| d.kind == PendingDeliveryKind::TriggerFire && d.key == key)
        .collect();
    assert_eq!(firings.len(), 1, "after 1: TriggerFire rows of T1");
    case.check("step 1").await;
    second.check("step 1").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn t03_only_the_due_time_trigger_fires() {
    let e0 = EvaluationId::now_v7();
    let [t1, t2, t3, t4] = [(); 4].map(|()| TaskTriggerId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evaluation(e0)
            .evaluation_status(EvaluationStatus::Completed)
            .trigger(
                t1,
                TriggerType::Polling,
                polling(3600, None),
                true,
                Some(ago(600)),
            )
            .trigger(
                t2,
                TriggerType::Time,
                time_trigger("0 0 0 1 1 *"),
                true,
                Some(ago(3600)),
            )
            .trigger(
                t3,
                TriggerType::Time,
                time_trigger("not a cron"),
                true,
                None,
            )
            .trigger(
                t4,
                TriggerType::Time,
                time_trigger("0 * * * * *"),
                true,
                Some(ago(120)),
            ),
    )
    .await;
    let key = TriggerFire {
        trigger: t4,
        last_fired_at: case.trigger(t4).await.last_fired_at,
    }
    .key();

    case.fire_once().await;
    let firings: Vec<_> = case
        .deliveries()
        .await
        .into_iter()
        .map(|d| (d.kind, d.key))
        .collect();
    assert_eq!(
        firings,
        vec![(PendingDeliveryKind::TriggerFire, key)],
        "after 1: pending_delivery"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t04_an_inactive_trigger_or_task_does_not_fire() {
    let (e0, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (t1, t2) = (TaskTriggerId::now_v7(), TaskTriggerId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evaluation(e0)
            .evaluation_status(EvaluationStatus::Completed)
            .trigger(t1, TriggerType::Polling, polling(60, None), false, None)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Completed)
            .task_option(|t| t.active = false)
            .trigger(t2, TriggerType::Polling, polling(60, None), true, None),
    )
    .await;

    case.fire_once().await;
    assert_eq!(case.deliveries().await, vec![], "after 1: pending_delivery");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t05_a_delivery_with_an_outdated_last_fired_at_applies_nothing() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(
        shape()
            .evaluation(e0)
            .evaluation_status(EvaluationStatus::Completed)
            .repository(&repo.url())
            .trigger(
                t1,
                TriggerType::Polling,
                polling(60, Some("main")),
                true,
                Some(ago(600)),
            ),
    )
    .await;
    let fired_at = case.trigger(t1).await.last_fired_at;
    let evaluations = case.evaluations().await;
    case.enqueue_trigger_fire(t1, Some(ago(1200))).await;

    assert_eq!(
        case.deliver_trigger_fires().await,
        1,
        "after 1: delivered firings"
    );
    assert_eq!(
        case.evaluations().await,
        evaluations,
        "after 1: evaluation rows unchanged"
    );
    assert_eq!(
        case.trigger(t1).await.last_fired_at,
        fired_at,
        "after 1: task_trigger(T1).last_fired_at"
    );
    let deliveries = case.deliveries().await;
    let [row] = deliveries.as_slice() else {
        panic!("after 1: a single pending_delivery row, got {deliveries:?}");
    };
    assert!(row.delivered_at.is_some(), "after 1: delivered_at set");
    assert_eq!(
        (row.attempts, row.last_error.as_deref()),
        (0, None),
        "after 1: attempts, last_error"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t06_a_delivery_on_a_new_commit_creates_a_queued_evaluation() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(polled(
        e0,
        t1,
        &repo,
        &c0,
        EvaluationStatus::Completed,
        ConcurrencyPolicy::SoftAbort,
    ))
    .await;

    case.fire_once().await;
    case.check("step 1").await;

    case.deliver_trigger_fires().await;
    let task = case.db.task_of(e0).await;
    let fresh = evaluations_besides(&case, &[e0]).await;
    let [e1] = fresh.as_slice() else {
        panic!("after 2: a single new evaluation, got {fresh:?}");
    };
    assert_eq!(
        (
            e1.task,
            e1.status,
            e1.trigger,
            e1.kind,
            e1.concurrent,
            e1.previous,
        ),
        (
            Some(task),
            EvaluationStatus::Queued,
            Some(t1),
            EvaluationKind::Normal,
            false,
            Some(e0),
        ),
        "after 2: E1"
    );
    assert_eq!(commit_of(&case, e1).await, c1, "after 2: E1.commit");
    let last_evaluation = case
        .db
        .find::<task::Entity>(task)
        .await
        .expect("the task of E0")
        .last_evaluation;
    assert_eq!(
        last_evaluation,
        Some(e1.id),
        "after 2: task.last_evaluation"
    );
    assert!(
        case.trigger(t1).await.last_fired_at.is_some_and(is_recent),
        "after 2: task_trigger(T1).last_fired_at ~now"
    );
    let deliveries = case.deliveries().await;
    let [row] = deliveries.as_slice() else {
        panic!("after 2: a single pending_delivery row, got {deliveries:?}");
    };
    assert!(row.delivered_at.is_some(), "after 2: delivered_at set");
    assert_eq!(row.attempts, 0, "after 2: attempts");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn t07_a_head_already_evaluated_creates_nothing_and_still_stamps() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(polled(
        e0,
        t1,
        &repo,
        &c1,
        EvaluationStatus::Completed,
        ConcurrencyPolicy::SoftAbort,
    ))
    .await;
    let evaluations = case.evaluations().await.len();

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    assert_eq!(
        case.evaluations().await.len(),
        evaluations,
        "after 1: no new evaluation"
    );
    assert!(
        case.trigger(t1).await.last_fired_at.is_some_and(is_recent),
        "after 1: task_trigger(T1).last_fired_at ~now"
    );
    assert_eq!(
        undelivered(&case).await,
        vec![],
        "after 1: delivery delivered"
    );
    case.check("step 1").await;

    case.fire_once().await;
    assert_eq!(
        undelivered(&case).await,
        vec![],
        "after 2: undelivered pending_delivery rows"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn t08_a_failed_head_resolution_still_stamps_the_trigger() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evaluation(e0)
            .evaluation_status(EvaluationStatus::Completed)
            .repository("/nonexistent/repository")
            .trigger(
                t1,
                TriggerType::Polling,
                polling(60, Some("main")),
                true,
                None,
            ),
    )
    .await;
    let evaluations = case.evaluations().await.len();

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    let fired_at = case.trigger(t1).await.last_fired_at;
    assert!(
        fired_at.is_some_and(is_recent),
        "after 1: task_trigger(T1).last_fired_at ~now"
    );
    assert_eq!(
        case.evaluations().await.len(),
        evaluations,
        "after 1: no evaluation"
    );
    let deliveries = case.deliveries().await;
    let [row] = deliveries.as_slice() else {
        panic!("after 1: a single pending_delivery row, got {deliveries:?}");
    };
    assert!(row.delivered_at.is_some(), "after 1: delivered_at set");
    assert_eq!(
        (row.attempts, row.last_error.as_deref()),
        (0, None),
        "after 1: attempts, last_error"
    );
    case.check("step 1").await;

    case.fire_once().await;
    assert_eq!(
        undelivered(&case).await,
        vec![],
        "after 2: undelivered pending_delivery rows"
    );
    case.check("step 2").await;

    assert_eq!(
        case.deliver_trigger_fires().await,
        0,
        "after 3: delivered firings"
    );
    assert_eq!(
        case.trigger(t1).await.last_fired_at,
        fired_at,
        "after 3: task_trigger(T1).last_fired_at"
    );
    assert_eq!(
        case.evaluations().await.len(),
        evaluations,
        "after 3: no evaluation"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn t09_the_second_consumer_of_a_firing_finds_the_stamp_moved() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(polled(
        e0,
        t1,
        &repo,
        &c0,
        EvaluationStatus::Completed,
        ConcurrencyPolicy::SoftAbort,
    ))
    .await;
    let second = case.second_instance().await;

    case.fire_once().await;
    case.check("step 1").await;
    second.check("step 1").await;

    second.deliver_trigger_fires().await;
    let fresh = evaluations_besides(&case, &[e0]).await;
    let [e1] = fresh.as_slice() else {
        panic!("after 2: a single new evaluation, got {fresh:?}");
    };
    assert_eq!(commit_of(&case, e1).await, c1, "after 2: E1.commit");
    let fired_at = case.trigger(t1).await.last_fired_at;
    assert!(
        fired_at.is_some_and(is_recent),
        "after 2: task_trigger(T1).last_fired_at ~now"
    );
    case.check("step 2").await;
    second.check("step 2").await;

    let deliveries = case.deliveries().await;
    let [row] = deliveries.as_slice() else {
        panic!("after 2: a single pending_delivery row, got {deliveries:?}");
    };
    case.deliver_trigger_fire_again(row).await;
    assert_eq!(
        evaluations_besides(&case, &[e0]).await.len(),
        1,
        "after 3: new evaluations"
    );
    assert_eq!(
        case.trigger(t1).await.last_fired_at,
        fired_at,
        "after 3: task_trigger(T1).last_fired_at"
    );
    case.check("step 3").await;
    second.check("step 3").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn t10_soft_abort_aborts_the_eval_job_and_leaves_the_running_build() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(
        polled(
            e0,
            t1,
            &repo,
            &c0,
            EvaluationStatus::Queued,
            ConcurrencyPolicy::SoftAbort,
        )
        .startable(A, A_OUT),
    )
    .await;
    let a = evaluate_and_build_a(&case, e0).await;

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    let fresh = evaluations_besides(&case, &[e0]).await;
    let [e1] = fresh.as_slice() else {
        panic!("after 1: a single new evaluation, got {fresh:?}");
    };
    assert_eq!(
        (commit_of(&case, e1).await, e1.status),
        (c1, EvaluationStatus::Queued),
        "after 1: E1"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == eval_key(e0)),
        "after 1: signals(W1) == [Abort{{eval:E0}}], got {signals:?}"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 1: active(build:A)"
    );
    assert_eq!(
        case.evaluation(e0).await.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E0).status"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t11_hard_abort_aborts_the_older_evaluation_like_the_api() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(hard_abort_seed(e0, t1, &repo, &c0)).await;
    let [a, b, c] = hard_abort_prefix(&case, e0).await;

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    let aborted = case.evaluation(e0).await;
    assert_eq!(
        aborted.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E0).status"
    );
    assert!(
        aborted.finished_at.is_some(),
        "after 1: evaluation(E0).finished_at set"
    );
    let fresh = evaluations_besides(&case, &[e0]).await;
    let [e1] = fresh.as_slice() else {
        panic!("after 1: a single new evaluation, got {fresh:?}");
    };
    assert_eq!(
        (commit_of(&case, e1).await, e1.status),
        (c1, EvaluationStatus::Queued),
        "after 1: E1"
    );
    let signals = case.signals(W1);
    for key in [eval_key(e0), a] {
        assert!(
            signals
                .iter()
                .any(|s| matches!(s, SessionSignal::Abort { job_id, .. } if *job_id == key)),
            "after 1: signals(W1) contains Abort{{{key}}}, got {signals:?}"
        );
    }
    let signals = case.signals(W2);
    assert!(
        signals
            .iter()
            .any(|s| matches!(s, SessionSignal::Abort { job_id, .. } if *job_id == c)),
        "after 1: signals(W2) contains Abort{{build:C}}, got {signals:?}"
    );
    assert!(!case.pending(&b).await, "after 1: pending(build:B)");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Aborted,
        "after 1: sb(B).status"
    );
    assert_eq!(case.import_waits(), 0, "after 1: import waits of r1");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t12_skip_drops_the_event_and_moves_the_stamp() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(
        polled(
            e0,
            t1,
            &repo,
            &c0,
            EvaluationStatus::Building,
            ConcurrencyPolicy::Skip,
        )
        .startable(A, A_OUT),
    )
    .await;
    let evaluations = case.evaluations().await.len();

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    assert_eq!(
        case.evaluations().await.len(),
        evaluations,
        "after 1: no new evaluation"
    );
    assert_eq!(
        case.evaluation(e0).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E0).status"
    );
    assert!(
        case.trigger(t1).await.last_fired_at.is_some_and(is_recent),
        "after 1: task_trigger(T1).last_fired_at ~now"
    );
    assert_eq!(
        undelivered(&case).await,
        vec![],
        "after 1: delivery delivered"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t13_all_runs_the_new_evaluation_beside_the_old_one() {
    let (e0, t1) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(
        polled(
            e0,
            t1,
            &repo,
            &c0,
            EvaluationStatus::Building,
            ConcurrencyPolicy::All,
        )
        .startable(A, A_OUT),
    )
    .await;

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    let fresh = evaluations_besides(&case, &[e0]).await;
    let [e1] = fresh.as_slice() else {
        panic!("after 1: a single new evaluation, got {fresh:?}");
    };
    assert_eq!(
        (e1.status, e1.concurrent),
        (EvaluationStatus::Queued, true),
        "after 1: E1"
    );
    assert_eq!(
        case.evaluation(e0).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E0).status"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t14_push_and_pull_request_triggers_do_not_fire_from_the_pass() {
    let e0 = EvaluationId::now_v7();
    let (t1, t2) = (TaskTriggerId::now_v7(), TaskTriggerId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evaluation(e0)
            .evaluation_status(EvaluationStatus::Completed)
            .trigger(
                t1,
                TriggerType::ReporterPush,
                serde_json::json!({}),
                true,
                None,
            )
            .trigger(
                t2,
                TriggerType::ReporterPullRequest,
                serde_json::json!({}),
                true,
                None,
            ),
    )
    .await;

    case.fire_once().await;
    assert_eq!(case.deliveries().await, vec![], "after 1: pending_delivery");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn t15_a_hard_abort_spares_a_build_a_live_third_evaluation_wants() {
    let (e0, e9, t1) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        TaskTriggerId::now_v7(),
    );
    let repo = GitRepo::new();
    let c0 = repo.commit("c0");
    let c1 = repo.commit("c1");
    repo.push("main", &c1);
    let case = SchedulerCase::start(
        hard_abort_seed(e0, t1, &repo, &c0)
            .evaluation(e9)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .entry_point(&attr_of(A))
            .job(),
    )
    .await;
    hard_abort_prefix(&case, e0).await;

    case.fire_once().await;
    case.deliver_trigger_fires().await;
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == eval_key(e0)),
        "after 1: signals(W1) == [Abort{{eval:E0}}], got {signals:?}"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e9).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E9).status"
    );
    case.check("step 1").await;

    case.finish().await;
}

fn due_polling(e0: EvaluationId, t1: TaskTriggerId, repo: &GitRepo) -> Shape {
    shape()
        .evaluation(e0)
        .evaluation_status(EvaluationStatus::Completed)
        .repository(&repo.url())
        .trigger(
            t1,
            TriggerType::Polling,
            polling(60, Some("main")),
            true,
            None,
        )
}

fn polled(
    e0: EvaluationId,
    t1: TaskTriggerId,
    repo: &GitRepo,
    commit: &[u8],
    status: EvaluationStatus,
    policy: ConcurrencyPolicy,
) -> Shape {
    shape()
        .evaluation(e0)
        .evaluation_status(status)
        .repository(&repo.url())
        .commit_hash(commit)
        .last_evaluation(e0)
        .task_option(|t| t.concurrency = policy)
        .trigger(
            t1,
            TriggerType::Polling,
            polling(60, Some("main")),
            true,
            None,
        )
        .into_cache(CacheId::now_v7())
        .worker_registration("w-reg")
}

fn hard_abort_seed(e0: EvaluationId, t1: TaskTriggerId, repo: &GitRepo, c0: &[u8]) -> Shape {
    polled(
        e0,
        t1,
        repo,
        c0,
        EvaluationStatus::Queued,
        ConcurrencyPolicy::HardAbort,
    )
    .startable(A, A_OUT)
    .startable(B, B_OUT)
    .startable(C, C_OUT)
}

async fn start_eval_job(case: &SchedulerCase, e0: EvaluationId) {
    case.evalrun(W1, e0).await;
    case.advance_evaluation(W1, &eval_key(e0), EvaluationStatus::EvaluatingDerivation)
        .await;
    case.admit_startable_moves().await;
}

async fn take_build(case: &SchedulerCase, worker: &str, key: &str) {
    let taken = case.request_job(worker, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(key.to_owned()),
        "step 0: {worker} takes {key}"
    );
    case.report_building(worker, key).await;
}

async fn evaluate_and_build_a(case: &SchedulerCase, e0: EvaluationId) -> String {
    case.connect(W1, w_all().slots(2), &[]).await;
    start_eval_job(case, e0).await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a = case.key(A).await;
    take_build(case, W1, &a).await;
    case.signals(W1);
    case.check("step 0").await;
    a
}

async fn hard_abort_prefix(case: &SchedulerCase, e0: EvaluationId) -> [String; 3] {
    case.connect(W1, w_all().slots(2), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    start_eval_job(case, e0).await;
    let (a, b, c) = (case.key(A).await, case.key(B).await, case.key(C).await);
    case.offers_full(W1).await;
    case.scores(
        W1,
        &[(&a, 0, 0, false), (&b, 5, FAR, false), (&c, 5, FAR, false)],
    )
    .await;
    case.offers_full(W2).await;
    case.scores(
        W2,
        &[(&a, 5, FAR, false), (&b, 5, FAR, false), (&c, 0, 0, false)],
    )
    .await;
    take_build(case, W1, &a).await;
    take_build(case, W2, &c).await;
    case.import_request(W1, &eval_key(e0), "r1", &[drv_path(C)])
        .await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 0").await;
    [a, b, c]
}

async fn evaluations_besides(
    case: &SchedulerCase,
    known: &[EvaluationId],
) -> Vec<evaluation::Model> {
    case.evaluations()
        .await
        .into_iter()
        .filter(|e| !known.contains(&e.id))
        .collect()
}

async fn commit_of(case: &SchedulerCase, evaluation: &evaluation::Model) -> Vec<u8> {
    case.db
        .find::<commit::Entity>(evaluation.commit)
        .await
        .expect("the commit of the evaluation")
        .hash
}

async fn undelivered(case: &SchedulerCase) -> Vec<pending_delivery::Model> {
    case.deliveries()
        .await
        .into_iter()
        .filter(|d| d.delivered_at.is_none())
        .collect()
}
