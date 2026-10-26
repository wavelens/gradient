/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::commit;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_types::vec_to_hex;
use gradient_types::waiting_reason::{EvalCapability, UnmetRequirement, WaitingReason};
use gradient_wire::types::{
    FlakeInputOverride, FlakeJob, FlakeSource, FlakeStep, InputUpdateSpec, JobKind,
};

use super::{
    A, A_OUT, ARM, B, B_OUT, C, C_OUT, CaseShapes, REPOSITORY, S_OUT, SchedulerCase, X86, ago,
    attr_of, drv_nar, eval_key, store_path, w_all, w_build, w_eval, w_network, w_network_eval,
    w_network_secrets,
};

const SSH_REPOSITORY: &str = "git+ssh://git@example.invalid/repo.git";

const W1: &str = "w1";
const W2: &str = "w2";
const W3: &str = "w3";

#[tokio::test]
async fn q01_a_queued_evaluation_becomes_one_pending_job_with_wildcards_and_overrides() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .wildcard("pkg.*")
            .input_override("nixpkgs", "github:a/b"),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    assert!(case.pending(&key).await, "after 1: pending(eval:E1)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W1)"
    );
    assert_eq!(case.assignments(&key).await, vec![], "after 1: dj == []");
    case.check("step 1").await;

    case.assign_queued_evals().await;
    assert_eq!(case.totals().await.pending, 1, "after 2: counts().pending");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    let offer = case.offers_full(W1).await;
    assert_eq!(
        candidate_keys(&offer),
        BTreeSet::from([key.clone()]),
        "after 3: result.candidates"
    );
    let job = flake_job(&case, &key).await;
    assert_eq!(
        (
            job.steps,
            job.source,
            job.wildcards,
            job.input_overrides,
            job.input_update
        ),
        (
            all_steps(),
            FlakeSource::Repository {
                url: REPOSITORY.into(),
                commit: commit_of(&case, e1).await,
            },
            vec!["pkg.*".to_owned()],
            vec![FlakeInputOverride {
                input_name: "nixpkgs".into(),
                url: Some("github:a/b".into()),
            }],
            None,
        ),
        "after 3: the job"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q02_an_evaluation_without_commit_or_without_task_is_skipped() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .without_commit()
            .evalq(e2)
            .without_task()
            .evalq(e3),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;

    case.assign_queued_evals().await;
    assert!(
        !case.pending(&eval_key(e1)).await,
        "after 1: pending(eval:E1)"
    );
    assert!(
        !case.pending(&eval_key(e2)).await,
        "after 1: pending(eval:E2)"
    );
    assert!(
        case.pending(&eval_key(e3)).await,
        "after 1: pending(eval:E3)"
    );
    assert_eq!(case.totals().await.pending, 1, "after 1: counts().pending");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn q03_an_evaluation_with_an_open_eval_row_is_not_picked_even_by_a_fresh_core() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .eval_assignment()
            .by_worker("worker")
            .dispatched_at(ago(600)),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    assert!(!case.pending(&key).await, "after 1: pending(eval:E1)");
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    case.check("step 1").await;

    case.respawn_core().await;
    case.assign_queued_evals().await;
    assert!(!case.pending(&key).await, "after 2: pending(eval:E1)");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn q04_an_idle_evaluation_only_worker_splits_the_pass_and_without_one_no_split() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    case.connect(W1, w_network_eval(), &[]).await;
    case.connect(W2, w_eval(), &[]).await;

    case.assign_queued_evals().await;
    for e in [e1, e2] {
        assert_eq!(
            flake_job(&case, &eval_key(e)).await.steps,
            vec![FlakeStep::FetchFlake],
            "after 1: steps of the pending job of {e}"
        );
    }
    case.check("step 1").await;

    case.seed(shape().evalq(e3)).await;
    case.score_all(W2).await;
    let taken = case.request_job(W2, JobKind::Flake).await;
    assert!(
        taken.is_none(),
        "after 2: result == nothing, got {:?}",
        taken.map(|a| a.job_id().to_owned())
    );
    case.check("step 2").await;

    case.disconnect(W2).await;
    case.assign_queued_evals().await;
    assert_eq!(
        flake_job(&case, &eval_key(e3)).await.steps,
        all_steps(),
        "after 3: steps of the pending job of E3"
    );
    for e in [e1, e2] {
        assert_eq!(
            flake_job(&case, &eval_key(e)).await.steps,
            vec![FlakeStep::FetchFlake],
            "after 3: steps of the pending job of {e} unchanged"
        );
    }
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q05_an_evaluation_only_worker_running_a_job_causes_no_split() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    case.connect(W1, w_network_eval(), &[]).await;
    case.connect(W2, w_eval(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

    case.offers_full(W1).await;
    case.score_all(W1).await;
    let fetch = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 2: W1 gets the fetch job of E1");
    assert_eq!(fetch.job_id(), key, "after 2: the fetch job of E1");
    case.report_flake_source(W1, &key, &store_path(S_OUT, "s"))
        .await;
    case.complete(W1, &key, fetch.assignment_id(), Vec::new(), 1)
        .await;
    case.check("step 2").await;

    case.offers_full(W2).await;
    case.score_all(W2).await;
    let follow_up = case.request_job(W2, JobKind::Flake).await;
    assert_eq!(
        follow_up.map(|a| a.job_id().to_owned()),
        Some(key.clone()),
        "after 3: W2 takes the follow-up of E1"
    );
    assert_eq!(
        case.active(&key).await.as_deref(),
        Some(W2),
        "after 3: active(eval:E1)"
    );
    case.check("step 3").await;

    case.seed(shape().evalq(e3)).await;
    case.assign_queued_evals().await;
    assert_eq!(
        flake_job(&case, &eval_key(e3)).await.steps,
        all_steps(),
        "after 4: the pending job of E3 has all three steps"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn q06_a_store_path_repository_yields_a_cached_source_that_still_fetches() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).repository("/nix/store/abc-source")).await;
    case.connect(W1, w_all(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    let offer = case.offers_full(W1).await;
    assert_eq!(
        candidate_keys(&offer),
        BTreeSet::from([key.clone()]),
        "after 1: result.candidates"
    );
    let job = flake_job(&case, &key).await;
    assert_eq!(
        (job.steps, job.source),
        (
            all_steps(),
            FlakeSource::Cached {
                store_path: "/nix/store/abc-source".into(),
            },
        ),
        "after 1: the candidate's job"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn q07_an_input_update_evaluation_is_queued_on_its_base_commit_with_the_update() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .input_update("c0", "g", &["nixpkgs"], false)
            .evaluation_kind(EvaluationKind::InputUpdate),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    let job = flake_job(&case, &key).await;
    assert_eq!(
        (job.source, job.input_update),
        (
            FlakeSource::Repository {
                url: REPOSITORY.into(),
                commit: "c0".into(),
            },
            Some(InputUpdateSpec {
                generator: "g".into(),
                inputs: vec!["nixpkgs".into()],
                discover_only: false,
            }),
        ),
        "after 1: job.source and job.input_update"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn q08_a_fetch_step_needs_network_and_an_evaluation_step_needs_eval() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    case.connect(W1, w_network(), &[]).await;
    case.connect(W2, w_eval(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

    let fetcher = case.offers_full(W1).await;
    let evaluator = case.offers_full(W2).await;
    assert_eq!(
        candidate_keys(&fetcher),
        BTreeSet::from([key.clone(), eval_key(e2)]),
        "after 2: offers(W1).candidates"
    );
    assert_eq!(
        candidate_keys(&evaluator),
        BTreeSet::new(),
        "after 2: offers(W2).candidates"
    );
    case.check("step 2").await;

    case.score_all(W1).await;
    let fetch = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 3: W1 gets the fetch job of E1");
    assert_eq!(fetch.job_id(), key, "after 3: the fetch job of E1");
    case.report_flake_source(W1, &key, &store_path(S_OUT, "s"))
        .await;
    case.complete(W1, &key, fetch.assignment_id(), Vec::new(), 1)
        .await;
    case.check("step 3").await;

    let fetcher = case.offers_delta(W1).await;
    let evaluator = case.offers_delta(W2).await;
    assert_eq!(
        candidate_keys(&fetcher),
        BTreeSet::new(),
        "after 4: offers(W1).candidates"
    );
    assert_eq!(
        candidate_keys(&evaluator),
        BTreeSet::from([key.clone()]),
        "after 4: offers(W2).candidates"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn q09_a_queued_evaluation_without_an_eval_worker_waits_until_one_connects() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    case.connect(W1, w_build(), &[]).await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(EvalCapability::Eval, 1)),
        ),
        "after 1: evaluation(E1)"
    );
    case.check("step 1").await;

    let updated_at = case.evaluation(e1).await.updated_at;
    case.refresh_waiting_state().await;
    assert_eq!(
        case.evaluation(e1).await.updated_at,
        updated_at,
        "after 2: evaluation(E1).updated_at unchanged"
    );
    case.check("step 2").await;

    case.connect(W2, w_all(), &[]).await;
    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (EvaluationStatus::Queued, None),
        "after 3: evaluation(E1)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q10_a_fetching_evaluation_without_a_network_worker_waits_on_network() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .evaluation_status(EvaluationStatus::Fetching),
    )
    .await;
    case.connect(W2, w_eval(), &[]).await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(EvalCapability::Network, 1)),
        ),
        "after 1: evaluation(E1)"
    );
    case.check("step 1").await;

    case.connect(W1, w_network_eval(), &[]).await;
    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (EvaluationStatus::Queued, None),
        "after 2: evaluation(E1)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn q11_a_building_evaluation_without_a_fitting_worker_waits_until_one_fits() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .one(e1, A, A_OUT)
            .system(ARM)
            .feature("big-parallel"),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;

    case.admit_startable_moves().await;
    case.refresh_waiting_state().await;
    let unmet = UnmetRequirement {
        architecture: ARM.into(),
        required_features: vec!["big-parallel".into()],
        build_count: 1,
    };
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::workers(vec![unmet], 1, vec![X86.into()])),
        ),
        "after 1: evaluation(E1)"
    );
    assert!(
        case.pending(&case.key(A).await).await,
        "after 1: pending(build:A)"
    );
    case.signals(W1);
    case.check("step 1").await;

    case.capabilities(W1, w_all().arch(&[X86, ARM]).features(&["big-parallel"]))
        .await;
    case.signals(W1);
    case.check("step 2").await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (EvaluationStatus::Building, None),
        "after 3: evaluation(E1)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q12_a_graph_stuck_evaluation_heals_once_the_drv_is_committed() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .probed()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::graph_stuck(1)),
        ),
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Created,
        "after 1: sb(A).status"
    );
    case.check("step 1").await;

    case.commit_nar(drv_nar(A)).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    case.check("step 2").await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (EvaluationStatus::Building, None),
        "after 3: evaluation(E1)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q13_a_server_drain_makes_evaluations_wait_and_its_end_returns_them() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).one(e2, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;

    case.drain_server(true).await;
    case.refresh_waiting_state().await;
    for e in [e1, e2] {
        assert_eq!(
            waiting_state(&case, e).await,
            (EvaluationStatus::Waiting, Some(WaitingReason::Draining)),
            "after 1: evaluation({e})"
        );
    }
    case.check("step 1").await;

    case.drain_server(false).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E2).status, its walk had finished"
    );
    case.check("step 2").await;

    case.refresh_waiting_state().await;
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Building,
        "after 3: evaluation(E2).status"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q14_the_waiting_pass_leaves_approval_and_no_cache_alone() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::approval(1, "author").to_json())
            .evalq(e2)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json()),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let before = (case.evaluation(e1).await, case.evaluation(e2).await);

    case.refresh_waiting_state().await;
    case.assign_queued_evals().await;
    assert_eq!(
        (case.evaluation(e1).await, case.evaluation(e2).await),
        before,
        "after 1: evaluation(E1), evaluation(E2) unchanged"
    );
    assert_eq!(case.totals().await.pending, 0, "after 1: pending == none");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn q15_a_long_graph_stuck_evaluation_spawns_one_drv_recovery_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::graph_stuck(1).to_json())
            .derivation(A)
            .walked()
            .probed()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT),
    )
    .await;
    case.backdate_evaluation(e1, ago(121)).await;
    let stuck = case.evaluation(e1).await;

    case.recover_drv_stuck_evals().await;
    let recoveries = recoveries_of(&case, e1).await;
    let [recovery] = recoveries.as_slice() else {
        panic!("after 1: a single recovery evaluation, got {recoveries:?}");
    };
    assert_eq!(
        (
            recovery.task,
            recovery.commit,
            recovery.kind,
            recovery.status,
            recovery.concurrent,
            recovery.previous,
        ),
        (
            stuck.task,
            stuck.commit,
            EvaluationKind::DrvRecovery,
            EvaluationStatus::Queued,
            false,
            Some(e1),
        ),
        "after 1: the recovery evaluation R"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E1).status"
    );
    case.check("step 1").await;

    case.recover_drv_stuck_evals().await;
    assert_eq!(
        recoveries_of(&case, e1).await.len(),
        1,
        "after 2: no second recovery evaluation"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn q16_the_reheal_thaws_a_timed_out_build_and_leaves_a_reproducible_failure() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::graph_stuck(1).to_json())
            .derivation(A)
            .walked()
            .probed()
            .drv_cached()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT)
            .depends_on(B)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .job()
            .output(B_OUT)
            .build_status(BuildStatus::FailedTimeout)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::WallClockTimeout),
            )
            .derivation(C)
            .walked()
            .probed()
            .drv_cached()
            .entry_point(&attr_of(C))
            .job()
            .output(C_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            ),
    )
    .await;

    case.reheal_graph_stuck_evals().await;
    let (a, b, c) = (case.sb(A).await, case.sb(B).await, case.sb(C).await);
    assert_eq!(
        (b.status, b.attempt),
        (BuildStatus::Queued, 0),
        "after 1: sb(B) status and attempt"
    );
    assert_eq!(
        c.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(C).status"
    );
    assert_eq!(a.status, BuildStatus::Created, "after 1: sb(A).status");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn q17_startup_recovery_keeps_open_rows_and_their_work() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = SchedulerCase::start(interrupted(e1, e2, e3)).await;
    let keys = [case.key(A).await, eval_key(e2), case.key(B).await];

    case.startup_recovery().await;
    for key in &keys {
        let closings: Vec<_> = case
            .assignments(key)
            .await
            .into_iter()
            .map(|row| (row.finished_at.is_some(), row.outcome))
            .collect();
        assert_eq!(
            closings,
            vec![(false, None)],
            "after 1: dj({key}) stays open"
        );
    }
    for path in [A, B] {
        let outcomes: Vec<_> = case
            .attempts(path)
            .await
            .into_iter()
            .map(|t| t.outcome)
            .collect();
        assert_eq!(
            outcomes,
            vec![AttemptOutcome::Running],
            "after 1: att({path})"
        );
    }
    assert_eq!(
        (case.sb(A).await.status, case.sb(B).await.status),
        (BuildStatus::Building, BuildStatus::Building),
        "after 1: sb(A).status, sb(B).status"
    );
    assert_eq!(
        (
            case.evaluation(e1).await.status,
            case.evaluation(e2).await.status,
            case.evaluation(e3).await.status,
        ),
        (
            EvaluationStatus::Building,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
        ),
        "after 1: evaluation(E1), evaluation(E2), evaluation(E3) status"
    );
    case.check("step 1").await;

    case.finish().await;
}

fn interrupted(e1: EvaluationId, e2: EvaluationId, e3: EvaluationId) -> Shape {
    shape()
        .one(e1, A, A_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .by_worker("w-old")
        .dispatched_at(ago(60))
        .evalq(e2)
        .evaluation_status(EvaluationStatus::EvaluatingDerivation)
        .eval_assignment()
        .by_worker("w-old")
        .dispatched_at(ago(60))
        .evaluation(e3)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(B)
        .walked()
        .probed()
        .entry_point(&attr_of(B))
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
}

fn all_steps() -> Vec<FlakeStep> {
    vec![
        FlakeStep::FetchFlake,
        FlakeStep::EvaluateFlake,
        FlakeStep::EvaluateDerivations,
    ]
}

fn candidate_keys(offer: &crate::actor::Offer) -> BTreeSet<String> {
    offer.candidates.iter().map(|c| c.job_id.clone()).collect()
}

async fn flake_job(case: &SchedulerCase, key: &str) -> FlakeJob {
    case.pending_flake_job(key)
        .await
        .unwrap_or_else(|| panic!("{key} is a pending eval job"))
}

async fn commit_of(case: &SchedulerCase, evaluation: EvaluationId) -> String {
    let id = case.evaluation(evaluation).await.commit;
    let row = case
        .db
        .find::<commit::Entity>(id)
        .await
        .unwrap_or_else(|| panic!("no commit {id}"));
    vec_to_hex(&row.hash)
}

async fn waiting_state(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> (EvaluationStatus, Option<WaitingReason>) {
    let row = case.evaluation(evaluation).await;
    (
        row.status,
        row.waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json),
    )
}

async fn recoveries_of(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> Vec<gradient_entity::evaluation::Model> {
    case.evaluations()
        .await
        .into_iter()
        .filter(|e| e.kind == EvaluationKind::DrvRecovery && e.previous == Some(evaluation))
        .collect()
}

#[tokio::test]
async fn q18_a_fetch_step_of_an_ssh_repository_needs_secrets() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).repository(SSH_REPOSITORY)).await;
    case.connect(W1, w_network(), &[]).await;
    case.connect(W2, w_network_secrets(), &[]).await;
    case.connect(W3, w_eval(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

    let without_secrets = case.offers_full(W1).await;
    let with_secrets = case.offers_full(W2).await;
    assert_eq!(
        candidate_keys(&case.offers_full(W3).await),
        BTreeSet::new(),
        "after 2: offers(W3).candidates"
    );
    assert_eq!(
        candidate_keys(&without_secrets),
        BTreeSet::new(),
        "after 2: offers(W1).candidates"
    );
    assert_eq!(
        candidate_keys(&with_secrets),
        BTreeSet::from([key.clone()]),
        "after 2: offers(W2).candidates"
    );
    case.check("step 2").await;

    case.score_all(W2).await;
    let fetch = case
        .request_job(W2, JobKind::Flake)
        .await
        .expect("after 3: W2 gets the fetch job of E1");
    assert_eq!(fetch.job_id(), key, "after 3: the fetch job of E1");
    case.report_flake_source(W2, &key, &store_path(S_OUT, "s"))
        .await;
    case.complete(W2, &key, fetch.assignment_id(), Vec::new(), 1)
        .await;
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn q19_a_fetching_evaluation_of_an_ssh_repository_waits_on_secrets() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .repository(SSH_REPOSITORY)
            .evaluation_status(EvaluationStatus::Fetching),
    )
    .await;
    case.connect(W1, w_network_eval(), &[]).await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(EvalCapability::Secrets, 1)),
        ),
        "after 1: evaluation(E1)"
    );
    case.check("step 1").await;

    case.connect(W2, w_network_secrets(), &[]).await;
    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (EvaluationStatus::Queued, None),
        "after 2: evaluation(E1)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn q20_a_build_without_an_upstream_answer_waits_for_the_upstream_caches() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::upstream_caches(1, Vec::new())),
        ),
        "after 1: evaluation(E1)"
    );
    case.check("step 1").await;

    case.finish().await;
}
