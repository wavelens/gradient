/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{self, AttemptFailureReason, AttemptOutcome};
use gradient_entity::derivation_build;
use gradient_entity::dispatched_job::{self, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, ProjectId};
use gradient_entity::worker_sample::WorkerSampleState;
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_types::waiting_reason::{EvalCapability, WaitingReason};
use gradient_wire::types::{BuildFailureKind, FlakeStep, Job, JobKind};

use super::{
    A, A_OUT, B, B_OUT, CaseShapes, SchedulerCase, SlotKind, Totals, ago, eval_key, failure,
    is_recent, output_nar, w_all, w_build, w_eval, w_network_build, w_network_eval,
};
use crate::actor::Offer;

const W1: &str = "w1";
const W2: &str = "w2";
const W9: &str = "w9";

const DEACTIVATED: &str = "project deactivated worker";

#[tokio::test]
async fn w01_connect_and_disconnect_open_and_close_connection_rows() {
    let case = SchedulerCase::start(shape()).await;
    let caps = serde_json::to_value(&w_all().caps).expect("the handshake flags as JSON");

    case.connect(W1, w_all(), &[]).await;
    let rows = case.connections(W1).await;
    let [row] = rows.as_slice() else {
        panic!("after 1: worker_connection(W1) has a single row, got {rows:?}");
    };
    assert!(is_recent(row.connected_at), "after 1: connected_at ~now");
    assert_eq!(
        (row.disconnected_at, &row.capabilities),
        (None, &caps),
        "after 1: worker_connection(W1) open with the handshake flags"
    );
    assert_eq!(case.totals().await.workers, 1, "after 1: counts().workers");
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    case.check("step 1").await;

    case.disconnect(W1).await;
    let rows = case.connections(W1).await;
    assert!(
        rows[0].disconnected_at.is_some_and(is_recent),
        "after 2: worker_connection(W1)[0].disconnected_at ~now, got {rows:?}"
    );
    assert_eq!(case.totals().await.workers, 0, "after 2: counts().workers");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Close { .. }]),
        "after 2: signals(W1) == [Close], got {signals:?}"
    );
    case.check("step 2").await;

    case.disconnect(W1).await;
    assert_eq!(
        case.connections(W1).await,
        rows,
        "after 3: worker_connection(W1) unchanged"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    case.check("step 3").await;

    case.connect(W1, w_all(), &[]).await;
    let rows = case.connections(W1).await;
    assert!(
        matches!(rows.as_slice(), [first, second]
            if first.disconnected_at.is_some() && second.disconnected_at.is_none()),
        "after 4: worker_connection(W1) has 2 rows, the second open, got {rows:?}"
    );
    assert_eq!(case.totals().await.workers, 1, "after 4: counts().workers");
    case.check("step 4").await;

    case.startup_recovery().await;
    let rows = case.connections(W1).await;
    assert!(
        rows.iter().all(|row| row.disconnected_at.is_some()),
        "after 5: the startup recovery closes the rows of the previous process, got {rows:?}"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn w02_a_worker_without_a_profile_is_offered_nothing_until_it_arrives() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_build().without_profile(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    assert_eq!(
        offered(&offer),
        Vec::<String>::new(),
        "after 1: result.candidates"
    );
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1), SessionSignal::Offers(2)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    let nothing = case.request_job(W1, JobKind::Build).await;
    assert!(nothing.is_none(), "after 2: result == nothing");
    assert_eq!(case.assignments_of(W1).await, vec![], "after 2: dj == []");
    case.check("step 2").await;

    case.capabilities(W1, w_all().slots(2)).await;
    case.check("step 3").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(
        offered(&offer),
        sorted(vec![a, b]),
        "after 4: offers result.candidates"
    );
    case.score_all(W1).await;
    let first = case.request_job(W1, JobKind::Build).await;
    let second = case.request_job(W1, JobKind::Build).await;
    assert!(
        first.is_some() && second.is_some(),
        "after 4: both requests give W1 a job"
    );
    let open = case
        .assignments_of(W1)
        .await
        .iter()
        .filter(|row| row.finished_at.is_none())
        .count();
    assert_eq!(open, 2, "after 4: dj has 2 open rows on W1");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn w03_capabilities_of_an_unknown_worker_have_no_effect() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    let a = case.key(A).await;

    case.capabilities(W9, w_all()).await;
    assert_eq!(
        case.connections(W9).await,
        vec![],
        "after 1: worker_connection == []"
    );
    assert_eq!(case.totals().await.workers, 0, "after 1: counts().workers");
    assert_eq!(case.signals(W9), vec![], "after 1: signals(W9)");
    case.check("step 1").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.signals(W9), vec![], "after 2: no Offers anywhere");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn w04_a_disconnect_while_building_requeues_the_build_for_another_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    for worker in [W1, W2] {
        case.offers_full(worker).await;
        case.score_all(worker).await;
    }
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    drain_signals(&case, &[W1, W2]);
    case.check("step 1").await;

    case.report_building(W1, &a).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 0, 1],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    case.disconnect(W1).await;
    let rows = case.assignments(&a).await;
    assert_eq!(
        closes(&rows),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 3: dj(build:A)"
    );
    assert!(
        rows[0].finished_at.is_some_and(is_recent),
        "after 3: dj(build:A) finished_at ~now"
    );
    let attempts = case.attempts(A).await;
    assert_eq!(
        verdicts(&attempts),
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 3: att(A)"
    );
    assert!(
        attempts[0].build_finished_at.is_some(),
        "after 3: att(A) build_finished_at set"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 3: counters(E1)"
    );
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    assert_eq!(case.active(&a).await, None, "after 3: active(build:A)");
    assert!(
        case.connections(W1).await[0].disconnected_at.is_some(),
        "after 3: worker_connection(W1)[0].disconnected_at set"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Close { .. }]),
        "after 3: signals(W1) == [Close], got {signals:?}"
    );
    assert_eq!(case.signals(W2), vec![], "after 3: signals(W2)");
    case.check("step 3").await;

    let offer = case.offers_delta(W2).await;
    assert_eq!(
        offered(&offer),
        vec![a.clone()],
        "after 4: offers result.candidates"
    );
    case.score_all(W2).await;
    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 4: W2 gets build:A")
        .assignment_id();
    assert_ne!(a2, a1, "after 4: result.assignment_id == a2");
    assert_eq!(
        verdicts(&case.attempts(A).await),
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 4: att(A)"
    );
    case.check("step 4").await;

    let before = state_of(&case, A).await;
    case.disconnect(W1).await;
    assert_eq!(state_of(&case, A).await, before, "after 5: nothing changed");
    assert_eq!(case.signals(W1), vec![], "after 5: signals(W1)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn w05_a_disconnect_before_the_building_report_leaves_no_attempt() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    for worker in [W1, W2] {
        case.offers_full(worker).await;
        case.score_all(worker).await;
    }
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.check("step 1").await;

    case.disconnect(W1).await;
    assert_eq!(
        closes(&case.assignments(&a).await),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.attempts(A).await, vec![], "after 2: att(A) == []");
    case.check("step 2").await;

    case.offers_delta(W2).await;
    case.score_all(W2).await;
    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 3: W2 gets build:A")
        .assignment_id();
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A) == []");
    case.check("step 3").await;

    case.report_building(W2, &a).await;
    assert_eq!(
        running(&case.attempts(A).await),
        vec![Some(a2)],
        "after 4: att(A) has a single Running row (dispatched_job a2)"
    );
    case.check("step 4").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W2, &a, a2, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 5: sb(A).status"
    );
    let last = case.attempts(A).await.pop().expect("after 5: att(A)");
    assert_eq!(
        (last.outcome, last.dispatched_job),
        (AttemptOutcome::Built, Some(a2)),
        "after 5: att(A)[-1]"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn w06_the_tenth_lost_worker_in_a_row_fails_the_build_and_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(lost_workers(shape().chain(e1), 9)).await;
    case.connect(W1, w_all(), &[]).await;
    let b = case.key(B).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:B");
    assert_eq!(assignment.job_id(), b, "after 1: result.job_id");
    let a1 = assignment.assignment_id();
    case.check("step 1").await;

    case.report_building(W1, &b).await;
    case.check("step 2").await;

    case.disconnect(W1).await;
    let attempts = case.attempts(B).await;
    assert_eq!(attempts.len(), 10, "after 3: att(B) has 10 rows");
    assert_eq!(
        verdicts(&attempts).pop(),
        Some((
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::WorkerLost)
        )),
        "after 3: att(B) newest"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedPermanent,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::DependencyFailed,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 3: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 2, 0, 0],
        "after 3: counters(E1)"
    );
    assert!(case.pending(&b).await, "after 3: pending(build:B)");
    assert_eq!(
        closes(&case.assignments(&b).await),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 3: dj(build:B)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w07_an_attempt_closed_for_another_reason_restarts_the_lost_worker_streak() {
    let e1 = EvaluationId::now_v7();
    let seed = lost_workers(shape().one(e1, A, A_OUT), 9)
        .attempt(AttemptOutcome::Failed, Some(AttemptFailureReason::Oom));
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.disconnect(W1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    let attempts = case.attempts(A).await;
    assert_eq!(attempts.len(), 11, "after 2: att(A) has 11 rows");
    assert_eq!(
        verdicts(&attempts).pop(),
        Some((
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )),
        "after 2: att(A) newest"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn w08_a_lost_eval_worker_sends_the_evaluation_to_waiting_until_an_eval_worker_connects() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let key = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    case.disconnect(W1).await;
    let no_eval_workers = WaitingReason::eval_workers(EvalCapability::Eval, 0).to_json();
    assert_eq!(
        closes(&case.assignments(&key).await),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 1: dj(eval:E1)"
    );
    let waiting = waiting_of(&case, e1).await;
    assert_eq!(
        waiting,
        (EvaluationStatus::Waiting, Some(no_eval_workers)),
        "after 1: evaluation(E1) Waiting on eval_workers"
    );
    assert_eq!(case.active(&key).await, None, "after 1: active(eval:E1)");
    case.check("step 1").await;

    case.refresh_waiting_state().await;
    assert_eq!(waiting_of(&case, e1).await, waiting, "after 2: unchanged");
    case.check("step 2").await;

    case.connect(W2, w_all(), &[]).await;
    case.check("step 3").await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_of(&case, e1).await,
        (EvaluationStatus::Queued, None),
        "after 4: evaluation(E1) Queued, waiting_reason NULL"
    );
    case.check("step 4").await;

    case.assign_queued_evals().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let assignment = case
        .request_job(W2, JobKind::Flake)
        .await
        .expect("after 5: W2 gets eval:E1");
    let a2 = assignment.assignment_id();
    let rows = case.assignments(&key).await;
    assert_eq!(
        closes(&rows),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 5: dj(eval:E1)"
    );
    assert_eq!(rows[1].worker_id, W2, "after 5: dj(eval:E1)[1].worker_id");
    let Job::Flake(flake) = &assignment.job else {
        panic!("after 5: a flake job, got {:?}", assignment.job);
    };
    assert_eq!(
        flake.steps,
        vec![
            FlakeStep::FetchFlake,
            FlakeStep::EvaluateFlake,
            FlakeStep::EvaluateDerivations
        ],
        "after 5: result.job.steps"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn w09_ten_eval_dispatches_fail_the_evaluation_and_nine_wait_for_eval_workers() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = closed_eval_rows(shape().evalq(e1), 4).evalq(e2);
    let case = SchedulerCase::start(closed_eval_rows(seed, 3)).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let (k1, k2) = (eval_key(e1), eval_key(e2));

    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let mut taken = Vec::new();
    for _ in 0..2 {
        let assignment = case
            .request_job(W1, JobKind::Flake)
            .await
            .expect("after 1: W1 gets an eval job");
        taken.push(assignment.job_id().to_owned());
    }
    assert_eq!(
        sorted(taken),
        sorted(vec![k1.clone(), k2.clone()]),
        "after 1: W1 runs eval:E1 and eval:E2"
    );
    case.check("step 1").await;

    case.report_status(W1, &k1, EvaluationStatus::Fetching)
        .await;
    case.report_status(W1, &k2, EvaluationStatus::Fetching)
        .await;
    case.check("step 2").await;

    case.disconnect(W1).await;
    let failed = case.evaluation(e1).await;
    assert_eq!(
        failed.status,
        EvaluationStatus::Failed,
        "after 3: evaluation(E1).status"
    );
    assert!(
        failed.finished_at.is_some(),
        "after 3: evaluation(E1).finished_at set"
    );
    let text = "evaluation was dispatched 10 times and each worker disconnected before reporting a result; giving up";
    let messages = case.messages(e1).await;
    assert!(
        messages.iter().any(|m| m.level == MessageLevel::Error
            && m.source.as_deref() == Some("scheduler")
            && m.message == text),
        "after 3: evaluation_message(E1) names the lost workers, got {messages:?}"
    );
    let waiting = case.evaluation(e2).await;
    assert_eq!(
        waiting.status,
        EvaluationStatus::Waiting,
        "after 3: evaluation(E2).status"
    );
    assert!(
        matches!(
            waiting
                .waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json),
            Some(WaitingReason::EvalWorkers { .. })
        ),
        "after 3: waiting_reason(E2).kind == eval_workers, got {:?}",
        waiting.waiting_reason
    );
    for (key, rows) in [(&k1, 10), (&k2, 9)] {
        let dispatched = case.assignments(key).await;
        assert_eq!(dispatched.len(), rows, "after 3: dj({key}) rows");
        assert!(
            dispatched.iter().all(|row| row.finished_at.is_some()),
            "after 3: every dj({key}) row closed"
        );
        assert_eq!(
            dispatched.last().and_then(|row| row.outcome),
            Some(DispatchedJobOutcome::Abandoned),
            "after 3: the newest dj({key}) row Abandoned"
        );
    }
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w10_a_worker_without_build_gets_no_substitute_job() {
    let case = SchedulerCase::start(shape().passthrough(EvaluationId::now_v7())).await;
    case.connect(W1, w_eval(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    assert_eq!(
        offered(&offer),
        Vec::<String>::new(),
        "after 1: result.candidates"
    );
    case.check("step 1").await;

    let nothing = case.request_job(W1, JobKind::Build).await;
    assert!(nothing.is_none(), "after 2: result == nothing");
    assert_eq!(case.assignments(&a).await, vec![], "after 2: dj == []");
    assert_eq!(case.attempts(A).await, vec![], "after 2: att(A) == []");
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn w11_a_network_worker_without_build_gets_the_download_job() {
    let seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .system("builtin")
        .fixed_output();
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_network_eval(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    assert_eq!(
        offered(&offer),
        vec![a.clone()],
        "after 1: offers result.candidates"
    );
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets download:A");
    assert_eq!(assignment.job_id(), a, "after 1: result.job_id");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn w12_a_draining_worker_keeps_its_job_and_gets_no_offers_and_no_new_job() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let first = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets a first job");
    let second = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets a second job");
    assert_eq!(
        sorted(vec![first.job_id().to_owned(), second.job_id().to_owned()]),
        sorted(vec![a.clone(), b.clone()]),
        "after 1: W1 runs A and B"
    );
    let (a1, a2) = if first.job_id() == a {
        (first.assignment_id(), second.assignment_id())
    } else {
        (second.assignment_id(), first.assignment_id())
    };
    drain_signals(&case, &[W1]);
    case.check("step 1").await;

    case.fail(
        W1,
        &b,
        a2,
        failure(BuildFailureKind::Canceled).elapsed_ms(1),
    )
    .await;
    assert!(!case.pending(&b).await, "after 2: pending(build:B)");
    assert_eq!(
        closes(&case.assignments(&b).await),
        vec![(a2, Some(DispatchedJobOutcome::Abandoned))],
        "after 2: dj(build:B)"
    );
    assert_eq!(case.attempts(B).await, vec![], "after 2: att(B) == []");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 2: sb(B).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.draining(W1).await;
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 3: active(build:A)"
    );
    assert!(
        !case
            .idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 3: idle_slots does not contain (W1, Build)"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    case.check("step 3").await;

    case.resync_startable_set().await;
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    let nothing = case.request_job(W1, JobKind::Build).await;
    assert!(nothing.is_none(), "after 5: result == nothing");
    assert!(case.pending(&b).await, "after 5: pending(build:B)");
    case.check("step 5").await;

    let before = (
        case.assignments_of(W1).await,
        case.pending(&b).await,
        case.active(&a).await,
        case.idle_slots().await,
    );
    case.draining(W1).await;
    let after = (
        case.assignments_of(W1).await,
        case.pending(&b).await,
        case.active(&a).await,
        case.idle_slots().await,
    );
    assert_eq!(after, before, "after 6: nothing changed");
    assert_eq!(case.signals(W1), vec![], "after 6: signals(W1)");
    case.check("step 6").await;

    case.report_building(W1, &a).await;
    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 7: sb(A).status"
    );
    assert_eq!(
        closes(&case.assignments(&a).await),
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 7: dj(build:A)"
    );
    case.check("step 7").await;

    case.finish().await;
}

#[tokio::test]
async fn w13_the_liveness_pass_drops_a_silent_worker_like_a_disconnect() {
    let seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    let case = SchedulerCase::start_with(seed, |c| c.proto.worker_heartbeat_timeout_secs = 2).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    for worker in [W1, W2] {
        case.offers_full(worker).await;
        case.score_all(worker).await;
    }
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    drain_signals(&case, &[W1, W2]);
    case.check("step 1").await;

    tokio::time::sleep(Duration::from_secs_f64(1.0)).await;
    case.heartbeat(W2);
    tokio::time::sleep(Duration::from_secs_f64(1.3)).await;

    case.worker_liveness().await;
    assert_eq!(case.totals().await.workers, 1, "after 3: counts().workers");
    assert!(
        case.connections(W1).await[0].disconnected_at.is_some(),
        "after 3: worker_connection(W1)[0].disconnected_at set"
    );
    assert_eq!(
        case.connections(W2).await[0].disconnected_at,
        None,
        "after 3: worker_connection(W2) open"
    );
    assert_eq!(
        closes(&case.assignments(&a).await),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 3: dj(build:A)"
    );
    assert_eq!(
        verdicts(&case.attempts(A).await),
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 3: att(A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Close { .. }]),
        "after 3: signals(W1) == [Close], got {signals:?}"
    );
    assert_eq!(case.signals(W2), vec![], "after 3: signals(W2)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w14_a_worker_silent_below_the_timeout_stays_connected() {
    let case =
        SchedulerCase::start_with(shape(), |c| c.proto.worker_heartbeat_timeout_secs = 2).await;
    case.connect(W1, w_all(), &[]).await;

    tokio::time::sleep(Duration::from_secs_f64(1.0)).await;
    case.worker_liveness().await;
    assert_eq!(case.totals().await.workers, 1, "after 1: counts().workers");
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn w14_a_zero_heartbeat_timeout_disables_the_liveness_pass() {
    let case =
        SchedulerCase::start_with(shape(), |c| c.proto.worker_heartbeat_timeout_secs = 0).await;
    case.connect(W1, w_all(), &[]).await;

    assert_eq!(
        case.liveness_period(),
        None,
        "after 2: no liveness pass runs"
    );
    assert_eq!(case.totals().await.workers, 1, "after 2: counts().workers");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    assert_eq!(
        case.connections(W1).await[0].disconnected_at,
        None,
        "after 2: worker_connection(W1) open"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn w15_reattached_jobs_are_active_again_after_a_core_respawn() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().two(e1).evalq(e2)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let (a, b, eval) = (case.key(A).await, case.key(B).await, eval_key(e2));

    case.admit_startable_moves().await;
    case.assign_queued_evals().await;
    for worker in [W1, W2] {
        case.offers_full(worker).await;
    }
    case.scores(
        W1,
        &[
            (a.as_str(), 0, 0, false),
            (b.as_str(), 5, 5_000_000, false),
            (eval.as_str(), 0, 0, false),
        ],
    )
    .await;
    case.score_all(W2).await;
    let build = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    assert_eq!(build.job_id(), a, "after 1: W1 takes the better scored A");
    let flake = case
        .request_job(W2, JobKind::Flake)
        .await
        .expect("after 1: W2 gets eval:E2");
    assert_eq!(flake.job_id(), eval, "after 1: W2 takes eval:E2");
    let (a1, a2) = (build.assignment_id(), flake.assignment_id());
    case.check("step 1").await;

    case.report_building(W1, &a).await;
    case.report_status(W2, &eval, EvaluationStatus::Fetching)
        .await;
    case.check("step 2").await;

    let rows = (case.assignments(&a).await, case.assignments(&eval).await);
    case.respawn_core().await;
    assert_eq!(
        case.totals().await,
        Totals {
            workers: 0,
            pending: 0,
            active: 0
        },
        "after 3: counts()"
    );
    case.check("step 3").await;

    case.reattach(W1, &[(&a, a1)]).await;
    case.reattach(W2, &[(&eval, a2)]).await;
    assert_eq!(
        (case.assignments(&a).await, case.assignments(&eval).await),
        rows,
        "after 4: dj(build:A), dj(eval:E2) unchanged"
    );
    assert!(
        rows.0
            .iter()
            .chain(&rows.1)
            .all(|r| r.finished_at.is_none()),
        "after 4: dj(build:A), dj(eval:E2) open"
    );
    for worker in [W1, W2] {
        assert_eq!(
            case.connections(worker).await.len(),
            1,
            "after 4: worker_connection({worker}) has no new rows"
        );
    }
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 4: active(build:A)"
    );
    assert_eq!(
        case.active(&eval).await.as_deref(),
        Some(W2),
        "after 4: active(eval:E2)"
    );
    assert!(!case.pending(&b).await, "after 4: pending(build:B)");
    assert_eq!(case.totals().await.active, 2, "after 4: counts().active");
    drain_signals(&case, &[W1, W2]);
    case.check("step 4").await;

    case.resync_startable_set().await;
    case.assign_queued_evals().await;
    assert!(case.pending(&b).await, "after 5: pending(build:B)");
    assert!(!case.pending(&eval).await, "after 5: pending(eval:E2)");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Offers(_)]),
        "after 5: signals(W1) == [Offers(_)], got {signals:?}"
    );
    case.check("step 5").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 6: sb(A).status"
    );
    assert_eq!(
        closes(&case.assignments(&a).await),
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 6: dj(build:A)"
    );
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn w16_reattaching_a_job_of_a_closed_row_does_not_reopen_the_row() {
    let seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .open_assignment()
        .by_worker(W1)
        .dispatched_at(ago(3600));
    let case = SchedulerCase::start(seed).await;
    let a = case.key(A).await;
    case.connect(W1, w_all(), &[]).await;
    let rows = case.assignments(&a).await;
    let [row] = rows.as_slice() else {
        panic!("after 0: dj(build:A) has a single row, got {rows:?}");
    };
    assert_eq!(
        row.outcome,
        Some(DispatchedJobOutcome::Abandoned),
        "after 0: dj(build:A) == [{{Abandoned}}]"
    );
    let seeded = row.id;
    case.check("step 0").await;

    case.respawn_core().await;
    case.check("step 1").await;

    case.reattach(W1, &[(&a, seeded)]).await;
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 2: active(build:A)"
    );
    case.check("step 2").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, seeded, Vec::new(), 5).await;
    assert_eq!(
        case.assignments(&a).await,
        rows,
        "after 3: dj(build:A) == [{{Abandoned}}] unchanged"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A) == []");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w17_each_sampling_pass_writes_a_row_per_connected_worker() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let caps = serde_json::to_value(&w_all().caps).expect("the handshake flags as JSON");

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    case.check("step 1").await;

    case.draining(W2).await;
    case.check("step 2").await;

    case.sample_workers().await;
    assert_eq!(
        case.worker_samples().await.len(),
        2,
        "after 3: worker_sample rows"
    );
    let w1: Vec<_> = case
        .samples(W1)
        .await
        .into_iter()
        .map(|s| {
            (
                s.assigned_jobs,
                s.max_concurrent_builds,
                s.state,
                s.capabilities,
            )
        })
        .collect();
    assert_eq!(
        w1,
        vec![(1, 2, WorkerSampleState::Active, caps)],
        "after 3: worker_sample of W1"
    );
    let w2: Vec<_> = case
        .samples(W2)
        .await
        .into_iter()
        .map(|s| (s.assigned_jobs, s.state))
        .collect();
    assert_eq!(
        w2,
        vec![(0, WorkerSampleState::Draining)],
        "after 3: worker_sample of W2"
    );
    case.check("step 3").await;

    case.disconnect(W2).await;
    case.sample_workers().await;
    let samples = case.worker_samples().await;
    assert_eq!(samples.len(), 3, "after 4: worker_sample rows");
    assert_eq!(
        samples.last().map(|s| s.worker_id.as_str()),
        Some(W1),
        "after 4: the new row is W1's"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn w18_revoking_a_project_aborts_its_active_job_and_closes_the_row() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT).one(e2, B, B_OUT)).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);
    case.connect(W1, w_all().slots(1), &[p1, p2]).await;
    case.connect(W2, w_all(), &[p2]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.scores(
        W1,
        &[(a.as_str(), 5, 5_000_000, false), (b.as_str(), 0, 0, false)],
    )
    .await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets a job");
    assert_eq!(
        assignment.job_id(),
        b,
        "after 1: W1 takes the better scored B"
    );
    let a1 = assignment.assignment_id();
    drain_signals(&case, &[W1, W2]);
    case.check("step 1").await;

    case.report_building(W1, &b).await;
    case.check("step 2").await;

    case.revoke(W1, &[p2]).await;
    assert!(case.pending(&b).await, "after 3: pending(build:B)");
    assert_eq!(
        closes(&case.assignments(&b).await),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 3: dj(build:B)"
    );
    let status = case.sb(B).await.status;
    assert!(
        matches!(status, BuildStatus::Building | BuildStatus::Queued),
        "after 3: sb(B).status == Building or Queued, got {status:?}"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [
            SessionSignal::Abort { job_id, reason },
            SessionSignal::Offers(_),
        ] if *job_id == b && reason == DEACTIVATED),
        "after 3: signals(W1) == [Abort{{build:B, {DEACTIVATED:?}}}, Offers(_)], got {signals:?}"
    );
    case.check("step 3").await;

    case.offers_delta(W2).await;
    case.score_all(W2).await;
    let taken = case.request_job(W2, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(b.clone()),
        "after 4: result.job_id"
    );
    assert_eq!(
        case.open_assignment(&b).await.map(|row| row.worker_id),
        Some(W2.to_owned()),
        "after 4: open(build:B).worker_id"
    );
    case.check("step 4").await;

    let offer = case.offers_full(W1).await;
    assert_eq!(offered(&offer), vec![a], "after 5: result.candidates");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn w19_a_revoke_without_active_jobs_only_narrows_the_offers() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT).one(e2, B, B_OUT)).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);
    case.connect(W1, w_all(), &[p1, p2]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    assert_eq!(
        offered(&offer),
        sorted(vec![a.clone(), b.clone()]),
        "after 1: result.candidates"
    );
    drain_signals(&case, &[W1]);
    case.check("step 1").await;

    case.revoke(W1, &[p1]).await;
    assert_eq!(case.assignments(&a).await, vec![], "after 2: dj(build:A)");
    assert_eq!(case.assignments(&b).await, vec![], "after 2: dj(build:B)");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    let offer = case.offers_full(W1).await;
    assert_eq!(
        offered(&offer),
        vec![b.clone()],
        "after 3: offers result.candidates"
    );
    case.score_all(W1).await;
    let taken = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(b),
        "after 3: request result.job_id"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w20_a_reauth_request_goes_only_to_a_connected_worker() {
    let case = SchedulerCase::start(shape()).await;
    case.connect(W1, w_all(), &[]).await;

    case.request_reauth(W1).await;
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Reauth],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.request_reauth(W9).await;
    assert_eq!(case.signals(W1), vec![], "after 2: no signal to W1");
    assert_eq!(case.signals(W9), vec![], "after 2: no signal to W9");
    case.check("step 2").await;

    case.disconnect(W1).await;
    case.request_reauth(W1).await;
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Close { .. }]),
        "after 3: signals(W1) == [Close] only, got {signals:?}"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w21_a_reauth_result_replaces_the_granted_projects() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT).one(e2, B, B_OUT)).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);
    case.connect(W1, w_all().slots(2), &[p1]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    assert_eq!(offered(&offer), vec![a], "after 1: result.candidates");
    drain_signals(&case, &[W1]);
    case.check("step 1").await;

    case.reauth_result(W1, &[p2]).await;
    case.check("step 2").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(
        offered(&offer),
        vec![b.clone()],
        "after 3: offers result.candidates"
    );
    case.score_all(W1).await;
    let first = case.request_job(W1, JobKind::Build).await;
    let second = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        first.as_ref().map(|j| j.job_id().to_owned()),
        Some(b),
        "after 3: the first request hands out B"
    );
    assert!(second.is_none(), "after 3: the second request gets nothing");
    let signals = case.signals(W1);
    assert!(
        !signals
            .iter()
            .any(|s| matches!(s, SessionSignal::Abort { .. })),
        "after 3: signals(W1) contains no Abort, got {signals:?}"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w22_a_reauth_result_without_a_granted_project_aborts_no_running_job() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    let p1 = case.db.project_of(e1).await;
    case.connect(W1, w_all(), &[p1]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    drain_signals(&case, &[W1]);
    case.check("step 1").await;

    let open = case.open_assignment(&a).await;
    case.reauth_result(W1, &[ProjectId::now_v7()]).await;
    assert_eq!(
        case.open_assignment(&a).await,
        open,
        "after 2: open(build:A) unchanged"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 2: active(build:A)"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn w23_a_fixed_output_build_needs_network() {
    only_a_network_worker_gets(shape().one(EvaluationId::now_v7(), A, A_OUT).fixed_output()).await;
}

#[tokio::test]
async fn w24_a_substitute_needs_network() {
    only_a_network_worker_gets(shape().passthrough(EvaluationId::now_v7())).await;
}

#[tokio::test]
async fn w25_a_download_needs_network() {
    only_a_network_worker_gets(
        shape()
            .one(EvaluationId::now_v7(), A, A_OUT)
            .system("builtin")
            .fixed_output(),
    )
    .await;
}

#[tokio::test]
async fn w26_a_reclaiming_worker_keeps_its_build_building_across_a_disconnect() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect_reclaiming(W1, w_all(), &[]).await;
    let (a, a1) = building_on_w1(&case).await;

    case.disconnect(W1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    assert_eq!(
        closes(&case.assignments(&a).await),
        vec![(a1, None)],
        "after 2: dj(build:A) stays open"
    );
    assert_eq!(
        running(&case.attempts(A).await),
        vec![Some(a1)],
        "after 2: att(A) still Running"
    );
    assert!(!case.pending(&a).await, "after 2: pending(build:A)");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Close { .. }]),
        "after 2: signals(W1) == [Close], got {signals:?}"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn w27_an_unreclaimed_build_is_requeued_with_worker_lost_after_the_grace() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect_reclaiming(W1, w_all(), &[]).await;
    let (a, a1) = building_on_w1(&case).await;
    case.disconnect(W1).await;

    case.requeue_unreclaimed_after(Duration::from_secs(599))
        .await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status inside the grace"
    );
    case.check("step 2").await;

    case.requeue_unreclaimed_after(Duration::from_secs(600))
        .await;
    assert_eq!(
        closes(&case.assignments(&a).await),
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 3: dj(build:A)"
    );
    assert_eq!(
        verdicts(&case.attempts(A).await),
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 3: att(A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.finish().await;
}

async fn building_on_w1(case: &SchedulerCase) -> (String, DispatchedJobId) {
    let a = case.key(A).await;
    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    drain_signals(case, &[W1]);
    case.check("step 1").await;
    (a, a1)
}

async fn only_a_network_worker_gets(seed: Shape) {
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_build(), &[]).await;
    case.connect(W2, w_network_build(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    let offline = case.offers_full(W1).await;
    let online = case.offers_full(W2).await;
    assert_eq!(
        offered(&offline),
        Vec::<String>::new(),
        "after 1: offers(W1).candidates"
    );
    assert_eq!(
        offered(&online),
        vec![a.clone()],
        "after 1: offers(W2).candidates"
    );
    case.check("step 1").await;

    let nothing = case.request_job(W1, JobKind::Build).await;
    assert!(nothing.is_none(), "after 2: W1 gets nothing");
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    case.check("step 2").await;

    case.score_all(W2).await;
    let job = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 3: W2 gets build:A");
    assert_eq!(job.job_id(), a, "after 3: result.job_id");
    case.check("step 3").await;

    case.finish().await;
}

fn drain_signals(case: &SchedulerCase, workers: &[&str]) {
    for worker in workers {
        case.signals(worker);
    }
}

fn sorted(mut keys: Vec<String>) -> Vec<String> {
    keys.sort();
    keys
}

fn offered(offer: &Offer) -> Vec<String> {
    sorted(offer.candidates.iter().map(|c| c.job_id.clone()).collect())
}

fn closes(rows: &[dispatched_job::Model]) -> Vec<(DispatchedJobId, Option<DispatchedJobOutcome>)> {
    rows.iter().map(|row| (row.id, row.outcome)).collect()
}

fn verdicts(
    attempts: &[build_attempt::Model],
) -> Vec<(AttemptOutcome, Option<AttemptFailureReason>)> {
    attempts.iter().map(|t| (t.outcome, t.reason)).collect()
}

fn running(attempts: &[build_attempt::Model]) -> Vec<Option<DispatchedJobId>> {
    attempts
        .iter()
        .filter(|t| t.outcome == AttemptOutcome::Running)
        .map(|t| t.dispatched_job)
        .collect()
}

async fn state_of(
    case: &SchedulerCase,
    path: &str,
) -> (
    Vec<dispatched_job::Model>,
    Vec<build_attempt::Model>,
    derivation_build::Model,
) {
    (
        case.assignments(&case.key(path).await).await,
        case.attempts(path).await,
        case.sb(path).await,
    )
}

async fn waiting_of(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> (EvaluationStatus, Option<serde_json::Value>) {
    let row = case.evaluation(evaluation).await;
    (row.status, row.waiting_reason)
}

fn lost_workers(seed: Shape, losses: usize) -> Shape {
    (0..losses).fold(seed, |seed, _| {
        seed.attempt(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost),
        )
    })
}

fn closed_eval_rows(seed: Shape, abandoned: usize) -> Shape {
    [
        (DispatchedJobOutcome::Completed, 3),
        (DispatchedJobOutcome::Failed, 2),
        (DispatchedJobOutcome::Abandoned, abandoned),
    ]
    .into_iter()
    .flat_map(|(outcome, rows)| std::iter::repeat_n(outcome, rows))
    .fold(seed, |seed, outcome| {
        seed.eval_assignment().closed(outcome, ago(3600))
    })
}
