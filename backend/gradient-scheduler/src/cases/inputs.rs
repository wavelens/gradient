/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{EvaluationId, ProjectId};
use gradient_fixtures::shape::shape;
use gradient_pool::session_port::SessionSignal;
use gradient_types::waiting_reason::{EvalCapability, WaitingReason};
use gradient_wire::types::{InputOutcome, JobKind};

use super::{
    A, A_OUT, CaseShapes, Profile, SchedulerCase, eval_key, output_nar, w_all, w_network_eval,
    w_network_secrets, without_offers,
};

const W1: &str = "w1";
const W2: &str = "w2";
const W3: &str = "w3";
const W4: &str = "w4";

const SSH_REQUEST: &str = r#"{"builtin":"fetchTree","args":{"type":"git","url":"git+ssh://git@example.invalid/input.git","rev":"abc"},"storePath":"/nix/store/aaaa-input"}"#;
const FETCHED: &str = r#"{"storePath":"/nix/store/aaaa-input"}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fetch {
    worker: String,
    fetch_id: String,
    project: ProjectId,
    request: String,
}

#[tokio::test]
async fn i01_an_input_request_goes_to_a_network_and_secrets_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, super::w_network(), &[]).await;
    case.connect(W3, w_fetch(), &[]).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let fetches = fetches_of(&case, &[W1, W2, W3]);
    assert_eq!(
        fetches,
        vec![Fetch {
            worker: W3.into(),
            fetch_id: fetches[0].fetch_id.clone(),
            project: case.db.project_of(e1).await,
            request: SSH_REQUEST.into(),
        }],
        "after 1: only W3 gets FetchInput"
    );
    assert_eq!(case.input_waits(), 1, "after 1: one input wait");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn i02_two_evaluations_share_one_fetch() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch(), &[]).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.input_request(W2, &eval_key(e2), "r2", SSH_REQUEST)
        .await;
    let fetches = fetches_of(&case, &[W1, W2, W3]);
    assert_eq!(fetches.len(), 1, "after 1: one FetchInput, got {fetches:?}");
    assert_eq!(case.input_waits(), 2, "after 1: two waiting requests");
    case.check("step 1").await;

    case.input_fetched(W3, &fetches[0].fetch_id, fetched())
        .await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 2: signals(W1)"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![result(e2, "r2", fetched())],
        "after 2: signals(W2)"
    );
    assert_eq!(case.input_waits(), 0, "after 2: no input wait left");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn i03_a_failed_fetch_answers_every_waiter() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch(), &[]).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.input_request(W2, &eval_key(e2), "r2", SSH_REQUEST)
        .await;
    let fetch = single_fetch(&case, &[W1, W2, W3]);
    case.check("step 1").await;

    let failed = InputOutcome::Failed {
        message: "HTTP error 404".into(),
    };
    case.input_fetched(W3, &fetch.fetch_id, failed.clone())
        .await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![result(e1, "r1", failed.clone())],
        "after 2: signals(W1)"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![result(e2, "r2", failed)],
        "after 2: signals(W2)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn i04_a_lost_fetch_worker_hands_the_fetch_to_another() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch(), &[]).await;
    case.connect(W3, w_fetch(), &[]).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(
        first.worker, W2,
        "after 1: the lower worker id wins the tie"
    );
    case.check("step 1").await;

    case.disconnect(W2).await;
    let second = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(second.worker, W3, "after 2: W3 takes over");
    assert_ne!(second.fetch_id, first.fetch_id, "after 2: a new fetch id");
    case.check("step 2").await;

    case.input_fetched(W2, &first.fetch_id, fetched()).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 3: the late answer of W2 is ignored"
    );
    case.check("step 3").await;

    case.input_fetched(W3, &second.fetch_id, fetched()).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 4: signals(W1)"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn i05_an_abort_drops_the_input_wait() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    assert_eq!(
        fetches_of(&case, &[W1]),
        vec![],
        "after 1: W1 lacks secrets"
    );
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    case.signals(W1);
    assert_eq!(case.input_waits(), 0, "after 2: the wait is gone");
    case.check("step 2").await;

    case.connect(W2, w_fetch(), &[]).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        fetches_of(&case, &[W1, W2]),
        vec![],
        "after 3: no FetchInput for an aborted evaluation"
    );
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 3: no InputResult"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn i06_without_a_fetch_worker_the_evaluation_waits_for_network() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.refresh_waiting_state().await;
    let row = case.evaluation(e1).await;
    assert_eq!(
        (
            row.status,
            row.waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json)
        ),
        (
            EvaluationStatus::EvaluatingDerivation,
            Some(WaitingReason::eval_workers(EvalCapability::Network, 1))
        ),
        "after 1: evaluation(E1) keeps evaluating and names the gap"
    );
    case.check("step 1").await;

    case.connect(W2, w_fetch(), &[]).await;
    case.build_assign_pass(false, false).await;
    let fetch = single_fetch(&case, &[W1, W2]);
    assert_eq!(fetch.worker, W2, "after 2: the new worker gets the fetch");
    let row = case.evaluation(e1).await;
    assert_eq!(
        (row.status, row.waiting_reason),
        (EvaluationStatus::EvaluatingDerivation, None),
        "after 2: evaluation(E1) has no waiting reason"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn i07_an_input_request_from_a_build_job_is_dropped() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_fetch(), &[]).await;
    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let build = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    case.signals(W1);
    case.check("step 1").await;

    case.input_request(W1, build.job_id(), "r1", SSH_REQUEST)
        .await;
    assert_eq!(fetches_of(&case, &[W1, W2]), vec![], "after 2: no fetch");
    assert_eq!(case.input_waits(), 0, "after 2: no wait opened");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn i08_fetch_progress_reaches_every_waiting_evaluation() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch(), &[]).await;
    case.connect(W4, w_fetch(), &[]).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.input_request(W2, &eval_key(e2), "r2", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2, W3, W4]);
    case.check("step 1").await;

    case.input_fetch_progress(&first.worker, &first.fetch_id, 512, Some(1024))
        .await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![progress(e1, "r1", 512, Some(1024))],
        "after 2: signals(W1)"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![progress(e2, "r2", 512, Some(1024))],
        "after 2: signals(W2)"
    );
    case.check("step 2").await;

    case.disconnect(&first.worker).await;
    let second = single_fetch(&case, &[W1, W2, W3, W4]);
    case.input_fetch_progress(&first.worker, &first.fetch_id, 1024, Some(1024))
        .await;
    for worker in [W1, W2] {
        assert_eq!(
            without_offers(case.signals(worker)),
            vec![],
            "after 3: progress of the replaced fetch reaches no {worker}"
        );
    }
    case.input_fetch_progress(&second.worker, &second.fetch_id, 64, None)
        .await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![progress(e1, "r1", 64, None)],
        "after 3: signals(W1)"
    );
    case.signals(W2);
    case.check("step 3").await;

    case.finish().await;
}

fn w_fetch() -> Profile {
    w_network_secrets().slots(1)
}

#[tokio::test]
async fn i09_an_input_fetch_waits_for_a_free_build_slot() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).one(e2, A, A_OUT)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_all(), &[]).await;
    let (a, a2) = building_on_w2(&case).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    assert_eq!(
        fetch_signals(&case, W2),
        vec![],
        "after 2: W2 has no free build slot"
    );
    assert_eq!(case.input_waits(), 1, "after 2: the request waits");
    case.check("step 2").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W2, &a, a2, Vec::new(), 5).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        fetch_signals(&case, W2).len(),
        1,
        "after 3: the freed slot takes the fetch"
    );
    case.signals(W1);
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn i10_a_running_fetch_takes_a_build_slot() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).one(e2, A, A_OUT)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_all(), &[]).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let fetches = fetch_signals(&case, W2);
    let [SessionSignal::FetchInput { fetch_id, .. }] = fetches.as_slice() else {
        panic!("after 1: W2 gets one FetchInput, got {fetches:?}");
    };
    case.check("step 1").await;

    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let taken = case.request_job(W2, JobKind::Build).await;
    assert!(
        taken.is_none(),
        "after 2: the fetch holds the only slot, got {:?}",
        taken.map(|t| t.job_id().to_owned())
    );
    case.check("step 2").await;

    case.input_fetched(W2, fetch_id, fetched()).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 3: signals(W1)"
    );
    let build = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 3: the released slot takes build:A");
    assert_eq!(build.job_id(), case.key(A).await, "after 3: build:A");
    case.signals(W2);
    case.check("step 3").await;

    case.finish().await;
}

async fn building_on_w2(case: &SchedulerCase) -> (String, gradient_entity::ids::DispatchedJobId) {
    let a = case.key(A).await;
    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let assignment = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 1: W2 gets build:A");
    assert_eq!(assignment.job_id(), a, "after 1: build:A");
    case.report_building(W2, &a).await;
    case.signals(W2);
    case.check("step 1").await;
    (a, assignment.assignment_id())
}

fn fetch_signals(case: &SchedulerCase, worker: &str) -> Vec<SessionSignal> {
    case.signals(worker)
        .into_iter()
        .filter(|s| matches!(s, SessionSignal::FetchInput { .. }))
        .collect()
}

async fn evaluating(case: &SchedulerCase, worker: &str, evaluation: EvaluationId) {
    let key = eval_key(evaluation);
    case.connect(worker, w_network_eval(), &[]).await;
    case.evalrun(worker, evaluation).await;
    case.advance_evaluation(worker, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.signals(worker);
    case.check("step 0").await;
}

fn fetches_of(case: &SchedulerCase, workers: &[&str]) -> Vec<Fetch> {
    let mut fetches = Vec::new();
    for worker in workers {
        for signal in without_offers(case.signals(worker)) {
            match signal {
                SessionSignal::FetchInput {
                    fetch_id,
                    project,
                    request,
                } => fetches.push(Fetch {
                    worker: (*worker).to_owned(),
                    fetch_id,
                    project,
                    request,
                }),
                other => panic!("{worker} got {other:?} besides FetchInput"),
            }
        }
    }
    fetches
}

fn single_fetch(case: &SchedulerCase, workers: &[&str]) -> Fetch {
    let mut fetches = fetches_of(case, workers);
    assert_eq!(fetches.len(), 1, "one FetchInput, got {fetches:?}");
    fetches.remove(0)
}

fn fetched() -> InputOutcome {
    InputOutcome::Fetched {
        response: FETCHED.into(),
    }
}

fn result(evaluation: EvaluationId, request_id: &str, outcome: InputOutcome) -> SessionSignal {
    SessionSignal::InputResult {
        job_id: eval_key(evaluation),
        request_id: request_id.into(),
        outcome,
    }
}

fn progress(
    evaluation: EvaluationId,
    request_id: &str,
    bytes_done: u64,
    bytes_total: Option<u64>,
) -> SessionSignal {
    SessionSignal::InputProgress {
        job_id: eval_key(evaluation),
        request_id: request_id.into(),
        bytes_done,
        bytes_total,
    }
}
