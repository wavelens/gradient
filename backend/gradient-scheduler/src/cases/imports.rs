/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId};
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_wire::types::{
    BuildFailureKind, DiscoveredDerivation, ImportOutcome, JobKind, Realisation,
};

use super::{
    CaseShapes, P, P_OUT, SchedulerCase, X, X_OUT, Y, Y_OUT, attr_of, build_output, drv, drv_path,
    eval_key, failure, output_nar, w_all, with_attr, with_ifd, with_output, without_offers,
};
use crate::job_handlers::import::NO_ENTRY_POINT;

const W1: &str = "w1";
const W2: &str = "w2";
const W3: &str = "w3";

#[tokio::test]
async fn f01_no_store_path_or_no_build_job_answers_unknown_and_opens_no_wait() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .derivation(P)
            .walked()
            .entry_point(&attr_of(P))
            .job()
            .output(P_OUT),
    )
    .await;
    evaluating(&case, e1, vec![entry(P, P_OUT, &[])]).await;
    let key = eval_key(e1);

    case.import_request(W1, &key, "r1", &["not-a-path".to_owned()])
        .await;
    assert_eq!(
        case.signals(W1),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Unknown {
                drv_path: "not-a-path".into(),
            }
        )],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    let before = (case.sb(P).await, case.evaluation(e1).await);
    case.import_request(W1, &key, "r2", &[drv_path(Y)]).await;
    assert_eq!(
        case.signals(W1),
        vec![answer(
            e1,
            "r2",
            ImportOutcome::Unknown {
                drv_path: drv_path(Y),
            }
        )],
        "after 2: signals(W1)"
    );
    assert_eq!(case.import_waits(), 0, "after 2: no pending wait");
    assert_eq!(
        (case.sb(P).await, case.evaluation(e1).await),
        before,
        "after 2: nothing written"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn f02_an_import_without_an_entry_point_in_the_evaluation_answers_failed() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .derivation(X)
            .walked()
            .probed()
            .drv_cached()
            .ifd()
            .job()
            .output(X_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;
    evaluating(&case, e1, vec![with_ifd(with_output(drv(X, &[]), X_OUT))]).await;

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    let failed = ImportOutcome::Failed {
        drv_path: drv_path(X),
        build_id: build_job_of(&case, e1, X).await,
        status: NO_ENTRY_POINT.into(),
    };
    assert_eq!(
        case.signals(W1),
        vec![answer(e1, "r1", failed)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn f03_an_import_of_a_completed_build_answers_completed_at_once() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        importing(e1, &[X], BuildStatus::Created)
            .derivation(X)
            .build_status(BuildStatus::Completed)
            .cached_output(X),
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    assert_eq!(
        case.signals(W1),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Completed {
                realisations: Vec::new()
            }
        )],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn f03b_a_completed_import_names_the_paths_its_build_realised() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        importing(e1, &[X], BuildStatus::Created)
            .derivation(X)
            .output_option(X_OUT, |o| o.path_known_after_build = true)
            .build_status(BuildStatus::Completed)
            .cached_output(X),
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    let realised = Realisation {
        drv_path: drv_path(X),
        output: "out".into(),
        path: format!(
            "/nix/store/{X_OUT}-{}",
            case.output_of(X, "out").await.package.unwrap_or_default()
        ),
    };
    assert_eq!(
        case.signals(W1),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Completed {
                realisations: vec![realised]
            }
        )],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn f03c_a_substituted_import_without_its_output_path_is_rebuilt_before_the_answer() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with_unstored_outputs(
        importing(e1, &[X], BuildStatus::Created)
            .derivation(X)
            .output_option(X_OUT, |o| {
                o.path_known_after_build = true;
                o.hash = None;
                o.package = None;
            })
            .build_status(BuildStatus::Substituted),
        1,
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;
    let x = case.key(X).await;
    assert_eq!(
        case.output_of(X, "out").await.hash,
        None,
        "after 0: output(X,out).hash"
    );

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::Queued,
        "after 1: sb(X).status"
    );
    case.set_unstored_outputs(0);
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    assert_eq!(case.import_waits(), 1, "after 1: import wait open");
    case.check("step 1").await;

    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:X")
        .assignment_id();
    case.report_building(W2, &x).await;
    let realised = build_output(X, X_OUT);
    case.report_build_output(W2, &x, vec![realised.clone()], true, None)
        .await;
    case.commit_nar(output_nar(X, X_OUT)).await;
    case.complete(W2, &x, a2, Vec::new(), 5).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        case.output_of(X, "out").await.hash.as_deref(),
        Some(X_OUT),
        "after 2: output(X,out).hash"
    );
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Completed {
                realisations: vec![Realisation {
                    drv_path: drv_path(X),
                    output: "out".into(),
                    path: realised.store_path,
                }]
            }
        )],
        "after 2: signals(W1)"
    );
    case.signals(W2);
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn f04_an_import_of_a_running_build_waits_and_answers_completed_exactly_once() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(startable_import(e1)).await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;

    let (x, a2) = claimed_by_w2(&case).await;
    waiting_import(&case, e1).await;

    case.commit_nar(output_nar(X, X_OUT)).await;
    case.complete(W2, &x, a2, Vec::new(), 5).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 3: signals(W1)"
    );
    case.check("step 3").await;

    case.build_assign_pass(false, false).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Completed {
                realisations: Vec::new()
            }
        )],
        "after 4: signals(W1)"
    );
    case.check("step 4").await;

    case.complete(W2, &x, a2, Vec::new(), 5).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 5: no second ImportResult"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn f05_an_import_of_an_aborted_build_requeues_it_and_answers_after_the_rebuild() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        importing(e1, &[X], BuildStatus::DependencyFailed)
            .derivation(X)
            .build_status(BuildStatus::Aborted)
            .job_option(|job| job.aborted = true)
            .attempt(AttemptOutcome::Aborted, None),
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;
    let x = case.key(X).await;

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    let sb = case.sb(X).await;
    assert_eq!(
        (sb.status, sb.attempt),
        (BuildStatus::Queued, 0),
        "after 1: sb(X) status and attempt"
    );
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    case.check("step 1").await;

    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:X")
        .assignment_id();
    case.report_building(W2, &x).await;
    case.commit_nar(output_nar(X, X_OUT)).await;
    case.complete(W2, &x, a2, Vec::new(), 5).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::Completed,
        "after 2: sb(X).status"
    );
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Completed {
                realisations: Vec::new()
            }
        )],
        "after 2: signals(W1)"
    );
    case.signals(W2);
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn f06_a_reproducible_builder_failure_is_not_requeued_and_answers_failed() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        importing(e1, &[X], BuildStatus::DependencyFailed)
            .derivation(X)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            ),
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(X).status"
    );
    assert_eq!(
        case.signals(W1),
        vec![answer(e1, "r1", failed(&case, e1, X).await)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn f07_two_paths_settle_when_both_are_terminal_and_the_first_failure_wins() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        importing(e1, &[X, Y], BuildStatus::Created)
            .derivation(X)
            .build_status(BuildStatus::Queued)
            .derivation(Y)
            .build_status(BuildStatus::Queued),
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT), (Y, Y_OUT)])).await;
    case.connect(W2, w_all().slots(2), &[]).await;
    let (x, y) = (case.key(X).await, case.key(Y).await);

    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let mut claimed = Vec::new();
    for _ in 0..2 {
        let assignment = case
            .request_job(W2, JobKind::Build)
            .await
            .expect("after 1: W2 gets build:X and build:Y");
        claimed.push((assignment.job_id().to_owned(), assignment.assignment_id()));
    }
    let a2 = assignment_of(&claimed, &x);
    let a3 = assignment_of(&claimed, &y);
    case.report_building(W2, &x).await;
    case.report_building(W2, &y).await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;

    case.import_request(W1, &eval_key(e1), "r1", &[drv_path(X), drv_path(Y)])
        .await;
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.fail(
        W2,
        &x,
        a2,
        failure(BuildFailureKind::Permanent).elapsed_ms(1),
    )
    .await;
    assert_eq!(
        (case.sb(X).await.status, case.sb(P).await.status),
        (BuildStatus::FailedPermanent, BuildStatus::DependencyFailed),
        "after 3: sb(X).status, sb(P).status"
    );
    case.check("step 3").await;

    case.build_assign_pass(false, false).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 4: signals(W1)"
    );
    case.check("step 4").await;

    case.commit_nar(output_nar(Y, Y_OUT)).await;
    case.complete(W2, &y, a3, Vec::new(), 1).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![answer(e1, "r1", failed(&case, e1, X).await)],
        "after 5: signals(W1)"
    );
    case.signals(W2);
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn f08_a_transient_failure_waits_through_the_retry_and_ends_on_the_permanent_failure() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        startable_import(e1)
            .derivation(X)
            .shared_build_option(|build| build.attempt = 2)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            )
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            ),
    )
    .await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;

    let (x, a2) = claimed_by_w2(&case).await;
    waiting_import(&case, e1).await;

    case.fail(
        W2,
        &x,
        a2,
        failure(BuildFailureKind::Transient).elapsed_ms(1),
    )
    .await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::FailedPermanent,
        "after 3: sb(X).status"
    );
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![answer(e1, "r1", failed(&case, e1, X).await)],
        "after 3: signals(W1)"
    );
    case.signals(W2);
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn f08b_a_transient_failure_with_attempts_left_keeps_the_import_waiting() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(startable_import(e1)).await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;

    let (x, a2) = claimed_by_w2(&case).await;
    waiting_import(&case, e1).await;

    case.fail(
        W2,
        &x,
        a2,
        failure(BuildFailureKind::Transient).elapsed_ms(1),
    )
    .await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::FailedTransient,
        "after 3: sb(X).status"
    );
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 3: signals(W1)"
    );
    case.signals(W2);
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn f09_aborting_the_importing_evaluation_drops_its_waits() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(startable_import(e1)).await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;
    let (x, a2) = claimed_by_w2(&case).await;
    waiting_import(&case, e1).await;
    let key = eval_key(e1);

    case.abort_evaluation(e1).await;
    let evaluator = without_offers(case.signals(W1));
    assert!(
        matches!(evaluator.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == key),
        "after 1: signals(W1) == [Abort{{eval:E1, _}}], got {evaluator:?}"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::Aborted,
        "after 1: sb(X).status"
    );
    let builder = without_offers(case.signals(W2));
    assert!(
        matches!(builder.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == x),
        "after 1: signals(W2) == [Abort{{build:X, _}}], got {builder:?}"
    );
    case.check("step 1").await;

    case.commit_nar(output_nar(X, X_OUT)).await;
    case.complete(W2, &x, a2, Vec::new(), 5).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![],
        "after 2: signals(W1)"
    );
    let outcomes: Vec<_> = case
        .assignments(&x)
        .await
        .into_iter()
        .map(|row| row.outcome)
        .collect();
    assert_eq!(
        outcomes.first().copied().flatten(),
        Some(DispatchedJobOutcome::Completed),
        "after 2: dj(build:X)[0].outcome"
    );
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::Aborted,
        "after 2: sb(X).status"
    );
    case.signals(W2);
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn f10_a_lost_evaluating_worker_gets_no_answer_and_the_rerun_gets_a_fresh_wait() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(startable_import(e1)).await;
    let a1 = evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;
    let (x, a2) = claimed_by_w2(&case).await;
    waiting_import(&case, e1).await;
    case.connect(W3, w_all(), &[]).await;
    let key = eval_key(e1);

    case.disconnect(W1).await;
    let rows: Vec<_> = case
        .assignments(&key)
        .await
        .into_iter()
        .map(|row| (row.id, row.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 1: dj(eval:E1)"
    );
    let status = case.evaluation(e1).await.status;
    assert!(
        matches!(status, EvaluationStatus::Waiting | EvaluationStatus::Queued),
        "after 1: evaluation(E1).status in {{Waiting, Queued}}, got {status:?}"
    );
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::Building,
        "after 1: sb(X).status"
    );
    case.check("step 1").await;

    case.refresh_waiting_state().await;
    case.assign_queued_evals().await;
    case.offers_full(W3).await;
    case.score_all(W3).await;
    let rerun = case
        .request_job(W3, JobKind::Flake)
        .await
        .expect("after 2: W3 gets the eval job of E1");
    assert_eq!(rerun.job_id(), key, "after 2: the eval job of E1");
    case.advance_evaluation(W3, &key, EvaluationStatus::EvaluatingFlake)
        .await;
    case.check("step 2 up to EvaluatingFlake").await;
    case.report_status(W3, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 2").await;

    case.import_request(W3, &key, "r1", &[drv_path(X)]).await;
    assert_eq!(
        without_offers(case.signals(W3)),
        vec![],
        "after 3: no ImportResult yet"
    );
    case.check("step 3").await;

    case.commit_nar(output_nar(X, X_OUT)).await;
    case.complete(W2, &x, a2, Vec::new(), 5).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        without_offers(case.signals(W3)),
        vec![answer(
            e1,
            "r1",
            ImportOutcome::Completed {
                realisations: Vec::new()
            }
        )],
        "after 4: signals(W3)"
    );
    let lost = case.signals(W1);
    assert!(
        matches!(lost.as_slice(), [SessionSignal::Close { .. }]),
        "after 4: signals(W1) == [Close{{_}}] only, got {lost:?}"
    );
    case.signals(W2);
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn f11_an_import_request_from_a_session_not_running_the_job_is_ignored() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(startable_import(e1)).await;
    evaluating(&case, e1, batch(&[(X, X_OUT)])).await;
    case.connect(W2, w_all(), &[]).await;
    let (x, a2) = claimed_by_w2(&case).await;
    case.connect(W3, w_all(), &[]).await;

    case.import_request(W3, &eval_key(e1), "r1", &[drv_path(X)])
        .await;
    for worker in [W1, W2, W3] {
        assert_eq!(
            without_offers(case.signals(worker)),
            vec![],
            "after 1: no ImportResult to {worker}"
        );
    }
    assert_eq!(case.import_waits(), 0, "after 1: no wait opened");
    case.commit_nar(output_nar(X, X_OUT)).await;
    case.complete(W2, &x, a2, Vec::new(), 5).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        without_offers(case.signals(W3)),
        vec![],
        "after 1: completing X sends no signal to W3"
    );
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;

    case.finish().await;
}

fn importing(evaluation: EvaluationId, imports: &[&str], entry_status: BuildStatus) -> Shape {
    let mut seed = shape().evalq(evaluation);
    for path in imports {
        seed = seed
            .derivation(path)
            .walked()
            .probed()
            .drv_cached()
            .ifd()
            .entry_point(&attr_of(path))
            .job()
            .output(output_of(path));
    }
    seed = seed
        .derivation(P)
        .walked()
        .entry_point(&attr_of(P))
        .job()
        .output(P_OUT)
        .build_status(entry_status);
    for path in imports {
        seed = seed.depends_on(path);
    }
    seed
}

fn startable_import(evaluation: EvaluationId) -> Shape {
    importing(evaluation, &[X], BuildStatus::Created)
        .derivation(X)
        .build_status(BuildStatus::Queued)
}

fn output_of(path: &str) -> &'static str {
    if path == Y { Y_OUT } else { X_OUT }
}

fn entry(path: &str, out: &str, dependencies: &[&str]) -> DiscoveredDerivation {
    with_attr(with_output(drv(path, dependencies), out), &attr_of(path))
}

fn batch(imports: &[(&str, &str)]) -> Vec<DiscoveredDerivation> {
    let dependencies: Vec<&str> = imports.iter().map(|(path, _)| *path).collect();
    let mut derivations = vec![entry(P, P_OUT, &dependencies)];
    derivations.extend(
        imports
            .iter()
            .map(|(path, out)| with_ifd(entry(path, out, &[]))),
    );
    derivations
}

async fn evaluating(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    derivations: Vec<DiscoveredDerivation>,
) -> DispatchedJobId {
    let key = eval_key(evaluation);
    let a1 = case.evalrun(W1, evaluation).await;
    case.advance_evaluation(W1, &key, EvaluationStatus::EvaluatingFlake)
        .await;
    case.check("step 0 up to EvaluatingFlake").await;
    case.report_status(W1, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.report_eval_result(W1, &key, derivations).await;
    case.signals(W1);
    case.check("step 0").await;
    a1
}

async fn claimed_by_w2(case: &SchedulerCase) -> (String, DispatchedJobId) {
    let x = case.key(X).await;
    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let assignment = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 1: W2 gets build:X");
    assert_eq!(assignment.job_id(), x, "after 1: result.job_id");
    case.report_building(W2, &x).await;
    let row = case
        .open_assignment(&x)
        .await
        .expect("after 1: open(build:X)");
    assert_eq!(
        row.job_context["ifd"],
        serde_json::json!(false),
        "after 1: open(build:X).job_context.ifd"
    );
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;
    (x, assignment.assignment_id())
}

async fn waiting_import(case: &SchedulerCase, evaluation: EvaluationId) {
    case.import_request(W1, &eval_key(evaluation), "r1", &[drv_path(X)])
        .await;
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    assert_eq!(case.import_waits(), 1, "after 2: import wait open");
    case.check("step 2").await;
}

fn answer(evaluation: EvaluationId, request_id: &str, outcome: ImportOutcome) -> SessionSignal {
    SessionSignal::ImportResult {
        job_id: eval_key(evaluation),
        request_id: request_id.into(),
        outcome,
    }
}

async fn failed(case: &SchedulerCase, evaluation: EvaluationId, path: &str) -> ImportOutcome {
    ImportOutcome::Failed {
        drv_path: drv_path(path),
        build_id: build_job_of(case, evaluation, path).await,
        status: format!("{:?}", case.sb(path).await.status),
    }
}

async fn build_job_of(case: &SchedulerCase, evaluation: EvaluationId, path: &str) -> String {
    case.job(evaluation, path)
        .await
        .unwrap_or_else(|| panic!("a build job of {path} in {evaluation}"))
        .id
        .to_string()
}

fn assignment_of(claimed: &[(String, DispatchedJobId)], key: &str) -> DispatchedJobId {
    claimed
        .iter()
        .find(|(job, _)| job == key)
        .map(|(_, assignment)| *assignment)
        .unwrap_or_else(|| panic!("{key} among the claimed jobs {claimed:?}"))
}
