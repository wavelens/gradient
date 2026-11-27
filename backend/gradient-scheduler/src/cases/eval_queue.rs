/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::store_path_content_variant_reader_choice::{
    StorePathContentVariantReader, StorePathInputClosureHashes,
    get_content_variants_served_by_binary_cache_for_store_path_hashes,
    get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache,
};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::commit;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_fixtures::shape::{Shape, shape};
use gradient_graph::NarCommit;
use gradient_types::vec_to_hex;
use gradient_types::waiting_reason::{EvalCapability, UnmetRequirement, WaitingReason};
use gradient_wire::types::{
    BuildFailureKind, FlakeInputOverride, FlakeJob, FlakeSource, FlakeStep, InputUpdateSpec,
    JobKind,
};

use super::{
    A, A_OUT, ARM, B, B_OUT, C, C_OUT, CaseShapes, D, D_OUT, REPOSITORY, S_OUT, SchedulerCase, X86,
    ago, attr_of, drv_nar, eval_key, failure, output_nar, store_path, w_all, w_build, w_eval,
    w_network, w_network_eval, w_network_secrets, with_references,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

const SSH_REPOSITORY: &str = "git+ssh://git@example.invalid/repo.git";
const HTTPS_REPOSITORY_URL_WITH_STORED_PROJECT_CREDENTIAL: &str =
    "https://example.invalid/repo.git";

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
    case.connect(W1, w_all()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    assert!(case.pending(&key).await, "after 1: pending(eval:E1)");
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    assert_eq!(case.assignments(&key).await, vec![], "after 1: dj == []");
    case.check("step 1").await;

    case.assign_queued_evals().await;
    assert_eq!(case.totals().await.pending, 1, "after 2: counts().pending");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

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
    case.connect(W1, w_all()).await;

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
    case.connect(W1, w_all()).await;
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
    case.connect(W1, w_network_eval()).await;
    case.connect(W2, w_eval()).await;

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
    case.connect(W1, w_network_eval()).await;
    case.connect(W2, w_eval()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

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
    case.connect(W1, w_all()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    let job = flake_job(&case, &key).await;
    assert_eq!(
        (job.steps, job.source),
        (
            all_steps(),
            FlakeSource::Cached {
                store_path: "/nix/store/abc-source".into(),
                commit: None,
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
    case.connect(W1, w_all()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
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
    case.connect(W1, w_network()).await;
    case.connect(W2, w_eval()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

    let evaluator = case.request_job(W2, JobKind::Flake).await;
    assert!(evaluator.is_none(), "after 2: W2 gets nothing");
    case.check("step 2").await;

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

    let evaluation = case.request_job(W2, JobKind::Flake).await;
    assert_eq!(
        evaluation.as_ref().map(|j| j.job_id().to_owned()),
        Some(key.clone()),
        "after 4: W2 gets the evaluation job of E1"
    );
    let fetch = case.request_job(W1, JobKind::Flake).await;
    assert_eq!(
        fetch.as_ref().map(|j| j.job_id().to_owned()),
        Some(eval_key(e2)),
        "after 4: W1 gets the fetch job of E2"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn q09_a_queued_evaluation_without_an_eval_worker_waits_until_one_connects() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    case.connect(W1, w_build()).await;

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

    case.connect(W2, w_all()).await;
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
    case.connect(W2, w_eval()).await;

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

    case.connect(W1, w_network_eval()).await;
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
    case.connect(W1, w_all()).await;

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
    case.connect(W1, w_all()).await;

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
    case.connect(W1, w_all()).await;

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
async fn q14_the_waiting_pass_leaves_full_cache_storage_and_a_still_missing_cache_alone() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::CacheStorageFull.to_json())
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json()),
    )
    .await;
    case.connect(W1, w_all()).await;
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
async fn q22_the_waiting_pass_names_a_missing_cache_before_missing_workers_and_resumes_with_one() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = SchedulerCase::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::eval_workers(EvalCapability::Eval, 0).to_json())
            .evalq(e2)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json())
            .evalq(e3)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json()),
    )
    .await;

    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e1).await,
        (EvaluationStatus::Waiting, Some(WaitingReason::NoCache)),
        "after 1: evaluation(E1) names the missing cache first"
    );
    assert_eq!(
        waiting_state(&case, e2).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(EvalCapability::Eval, 0)),
        ),
        "after 1: evaluation(E2) has a cache and now waits for workers"
    );
    case.check("step 1").await;

    case.connect(W1, w_all()).await;
    case.refresh_waiting_state().await;
    assert_eq!(
        waiting_state(&case, e2).await,
        (EvaluationStatus::Queued, None),
        "after 2: evaluation(E2)"
    );
    assert_eq!(
        waiting_state(&case, e3).await,
        (EvaluationStatus::Queued, None),
        "after 2: evaluation(E3)"
    );
    case.check("step 2").await;

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

#[tokio::test]
async fn q23_failed_drv_recovery_insertion_does_not_abort_stuck_evaluation_and_next_pass_creates_recovery()
 {
    use sea_orm::ConnectionTrait;

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

    let db = case.db.connection();
    db.execute_unprepared(
        "CREATE FUNCTION refuse_evaluation_insert() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 'refused'; END $$; \
         CREATE TRIGGER refuse_evaluation_insert BEFORE INSERT ON evaluation \
         FOR EACH ROW EXECUTE FUNCTION refuse_evaluation_insert();",
    )
    .await
    .expect("the trigger installs");
    case.recover_drv_stuck_evals().await;
    assert!(
        recoveries_of(&case, e1).await.is_empty(),
        "after 1: no recovery evaluation"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Waiting,
        "after 1: evaluation(E1).status"
    );

    db.execute_unprepared("DROP TRIGGER refuse_evaluation_insert ON evaluation;")
        .await
        .expect("the trigger drops");
    case.recover_drv_stuck_evals().await;
    assert_eq!(
        recoveries_of(&case, e1).await.len(),
        1,
        "after 2: one recovery evaluation"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn q24_blocked_unproducible_drv_does_not_spawn_recovery_when_cache_available_nar_present_deps_blocking_or_building()
 {
    let variants: [fn(Shape) -> Shape; 4] = [
        |shape| shape.cache_available().build_status(BuildStatus::Queued),
        |shape| shape.drv_cached().build_status(BuildStatus::Queued),
        |shape| shape.build_status(BuildStatus::Building),
        |shape| {
            shape
                .depends_on(B)
                .derivation(B)
                .walked()
                .probed()
                .output(B_OUT)
                .build_status(BuildStatus::Building)
        },
    ];
    for apply in variants {
        let e1 = EvaluationId::now_v7();
        let case = SchedulerCase::start(apply(
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
        ))
        .await;
        case.backdate_evaluation(e1, ago(121)).await;

        case.recover_drv_stuck_evals().await;

        assert!(recoveries_of(&case, e1).await.is_empty());
        case.finish().await;
    }
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
    case.connect(W1, w_network()).await;
    case.connect(W2, w_network_secrets()).await;
    case.connect(W3, w_eval()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

    let evaluator = case.request_job(W3, JobKind::Flake).await;
    assert!(evaluator.is_none(), "after 2: W3 gets nothing");
    let without_secrets = case.request_job(W1, JobKind::Flake).await;
    assert!(without_secrets.is_none(), "after 2: W1 gets nothing");
    case.check("step 2").await;

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
async fn https_repository_flake_job_needs_worker_with_secrets() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .repository(HTTPS_REPOSITORY_URL_WITH_STORED_PROJECT_CREDENTIAL),
    )
    .await;
    gradient_db::project_git_https_credentials::upsert(
        &case.db.connection(),
        case.db.project_of(e1).await,
        "example.invalid",
        "x-access-token",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();
    case.connect(W1, w_network()).await;
    case.connect(W2, w_network_secrets()).await;
    case.connect(W3, w_eval()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.check("step 1").await;

    let evaluator = case.request_job(W3, JobKind::Flake).await;
    assert!(evaluator.is_none(), "after 2: W3 gets nothing");
    let without_secrets = case.request_job(W1, JobKind::Flake).await;
    assert!(without_secrets.is_none(), "after 2: W1 gets nothing");
    case.check("step 2").await;

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
async fn test_unapproved_fork_pull_request_https_evaluation_withholds_project_secrets_and_git_credentials_and_names_withheld_credentials_in_failure_reason()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .repository(HTTPS_REPOSITORY_URL_WITH_STORED_PROJECT_CREDENTIAL)
            .mark_not_allowed_project_credentials(),
    )
    .await;
    gradient_db::project_git_https_credentials::upsert(
        &case.db.connection(),
        case.db.project_of(e1).await,
        "example.invalid",
        "x-access-token",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();
    case.connect(W1, w_network_eval()).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    let fetch = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 1: W1 without secrets gets the fetch job of E1");
    assert_eq!(fetch.job_id(), key, "after 1: the fetch job of E1");
    case.check("step 1").await;

    case.fail(
        W1,
        &key,
        fetch.assignment_id(),
        failure(BuildFailureKind::Permanent).error("clone failed"),
    )
    .await;
    let withheld = crate::exec_secrets::SecretResolutionError::ProjectGitCredentialsWithheldForUnapprovedForkPullRequest
        .to_string();
    let errors: Vec<String> = case
        .messages(e1)
        .await
        .into_iter()
        .filter(|m| m.level == MessageLevel::Error)
        .map(|m| m.message)
        .collect();
    assert!(
        errors
            .iter()
            .any(|m| m.contains("clone failed") && m.contains(&withheld)),
        "after 2: evaluation(E1) error names the withheld credentials, got {errors:?}"
    );
    case.check("step 2").await;

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
    case.connect(W1, w_network_eval()).await;

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

    case.connect(W2, w_network_secrets()).await;
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
    case.connect(W1, w_all()).await;

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

fn make_fixture_with_dependency_output_stored_only_as_user_upload_and_waiting_dependent_build_in_project_not_reading_cache(
    eq: EvaluationId,
    ep: EvaluationId,
    unread_cache: CacheId,
) -> Shape {
    shape()
        .evaluation(eq)
        .evaluation_status(EvaluationStatus::Completed)
        .derivation(D)
        .output(D_OUT)
        .job()
        .build_status(BuildStatus::Completed)
        .shared_build_option(|b| b.fetchable = true)
        .into_cache(unread_cache)
        .cached_path(D_OUT, "d")
        .mark_uploaded_by_user()
        .evaluation(ep)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(A))
        .job()
        .build_status(BuildStatus::Queued)
        .output(A_OUT)
        .depends_on(D)
}

async fn run_publish_pass_and_graph_consistency_check_until_settled(case: &SchedulerCase) {
    let publish_round = case
        .graph
        .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
            None,
            gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await;
    assert!(
        publish_round.failure.is_none(),
        "Run publish pass: {:?}",
        publish_round.failure
    );
    case.graph
        .run_consistency_checks()
        .await
        .expect("run periodic checks of graph consistency");
    case.settled().await;
}

#[tokio::test]
async fn build_dependency_output_only_in_unread_cache_is_not_signed_or_served() {
    let (eq, ep) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (unread_cache, signing_cache_id) = (CacheId::now_v7(), CacheId::now_v7());
    let case = SchedulerCase::start(
        make_fixture_with_dependency_output_stored_only_as_user_upload_and_waiting_dependent_build_in_project_not_reading_cache(
            eq,
            ep,
            unread_cache,
        )
        .into_cache(signing_cache_id)
        .keyed(SchedulerCase::signing_key()),
    )
    .await;
    let project = case.db.project_of(ep).await;

    run_publish_pass_and_graph_consistency_check_until_settled(&case).await;
    assert_eq!(
        case.db
            .signatures(D_OUT)
            .await
            .iter()
            .filter(|row| row.cache == signing_cache_id)
            .count(),
        0,
        "after 1: the cache of the EP project has no signed or placeholder signature row for D_OUT"
    );
    assert!(
        get_content_variants_served_by_binary_cache_for_store_path_hashes(
            &case.db.connection(),
            signing_cache_id,
            &[D_OUT.to_owned()],
        )
        .await
        .expect("served copies are readable")
        .remove(D_OUT)
        .is_none(),
        "after 1: the cache for the EP project does not hand out copies of D_OUT to outside readers."
    );
    case.check("step 1").await;

    case.connect(W1, w_all()).await;
    case.refresh_waiting_state().await;
    case.build_assign_pass(false, false).await;
    assert!(
        case.pending(&case.key(A).await).await,
        "after 2: A single assign pass offers build:A. The seed check of SchedulerCase::start must compare against the declared status Queued. Production runs also record build:A as Queued. This value is the target of the check, not a free choice of the fixture."
    );
    let assignment_for_w1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 2: W1 gets a build job");
    assert_eq!(
        assignment_for_w1.job_id(),
        case.key(A).await,
        "after 2: build job from W1 request_job = build:A"
    );
    assert_eq!(
        waiting_state(&case, ep).await.1,
        None,
        "after 2: no waiting reason on evaluation(EP) right after W1's request_job"
    );
    let assignment_a = assignment_for_w1.assignment_id();
    assert_eq!(
        case.sb(A).await.blocking_deps,
        0,
        "after 2: sb(A).blocking_deps"
    );
    assert_eq!(
        case.assignments(&case.key(A).await)
            .await
            .iter()
            .map(|job| job.worker_id.clone())
            .collect::<Vec<_>>(),
        vec![W1.to_owned()],
        "after 2: exactly one pass of build:A to W1"
    );
    assert_eq!(
        waiting_state(&case, ep).await,
        (EvaluationStatus::Building, None),
        "after 2: evaluation(EP)"
    );
    let conn = case.db.connection();
    assert_eq!(
        get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
            &conn,
            StorePathContentVariantReader::SharedBuild(
                case.sb(A).await.id,
                StorePathInputClosureHashes::default(),
            ),
            &[D_OUT.to_owned()],
        )
        .await
        .expect("execution of the build reader")
        .remove(D_OUT)
        .map(|row| row.hash),
        Some(D_OUT.to_owned()),
        "after 2: the reader for build:A -> the uploaded copy of D_OUT"
    );
    assert!(
        get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
            &conn,
            StorePathContentVariantReader::Project(project),
            &[D_OUT.to_owned()],
        )
        .await
        .expect("The project reader ran.")
        .remove(D_OUT)
        .is_none(),
        "after 2: the reader for EP's project doesn't return a copy of D_OUT"
    );
    case.check("step 2").await;

    case.commit_nar(NarCommit {
        producer: case.make_worker_producer_for_evaluation_project(ep).await,
        ..with_references(output_nar(A, A_OUT), &[(D_OUT, "d")])
    })
    .await;
    case.complete(W1, &case.key(A).await, assignment_a, Vec::new(), 5)
        .await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    run_publish_pass_and_graph_consistency_check_until_settled(&case).await;
    let project_signing_cache_has_signature_row_for_store_path_hash = async |hash: &str| {
        case.db
            .signatures(hash)
            .await
            .iter()
            .filter(|row| row.cache == signing_cache_id)
            .map(|row| row.signature.is_some())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (
            project_signing_cache_has_signature_row_for_store_path_hash(A_OUT).await,
            project_signing_cache_has_signature_row_for_store_path_hash(D_OUT).await,
        ),
        (vec![true], Vec::new()),
        "after 3: the cache of EP's project has a signature row for A_OUT and D_OUT that A_OUT references has no row there."
    );
    assert!(
        get_content_variants_served_by_binary_cache_for_store_path_hashes(
            &conn,
            signing_cache_id,
            &[D_OUT.to_owned()],
        )
        .await
        .expect("served copies are readable")
        .remove(D_OUT)
        .is_none(),
        "after 3: The cache for EP's project can't provide either the narinfo or the NAR for D_OUT"
    );
    assert!(
        get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
            &conn,
            StorePathContentVariantReader::Cache(signing_cache_id),
            &[D_OUT.to_owned()],
        )
        .await
        .expect("execution of the cache reader")
        .remove(D_OUT)
        .is_none(),
        "after 3: outside reader of EP's project cache doesn't get a copy of D_OUT"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn unread_cache_waiting_reason_is_replaced_by_waiting_refresh() {
    let ep = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evaluation(ep)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(D)
            .walked()
            .probed()
            .job()
            .output(D_OUT)
            .derivation(A)
            .walked()
            .probed()
            .drv_cached()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT)
            .depends_on(D),
    )
    .await;
    let removed_kind = serde_json::json!({
        "kind": "unread_cache",
        "waiting_build_count_on_dependency_in_unread_cache": 1,
    });
    gradient_entity::evaluation::Entity::update_many()
        .col_expr(
            gradient_entity::evaluation::Column::Status,
            sea_orm::sea_query::Expr::value(EvaluationStatus::Waiting),
        )
        .col_expr(
            gradient_entity::evaluation::Column::WaitingReason,
            sea_orm::sea_query::Expr::value(removed_kind.clone()),
        )
        .filter(gradient_entity::evaluation::Column::Id.eq(ep))
        .exec(&case.db.connection())
        .await
        .expect("The refresh replaced the stored waiting reason.");
    case.connect(W1, w_all()).await;

    case.refresh_waiting_state().await;
    let stored = case.evaluation(ep).await;
    assert_eq!(
        stored.status,
        EvaluationStatus::Waiting,
        "after 1: evaluation(EP).status"
    );
    let stored = stored
        .waiting_reason
        .expect("after 1: evaluation(EP) saved the waiting reason");
    assert_ne!(stored, removed_kind, "after 1: removed kind overwritten");
    assert!(
        WaitingReason::from_json(&stored).is_some(),
        "after 1: the stored reason {stored} decodes"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn q21_a_queued_evaluation_whose_caches_are_all_full_waits_instead_of_dispatching() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).full()).await;
    case.connect(W1, w_all()).await;

    case.assign_queued_evals().await;
    assert!(
        !case.pending(&eval_key(e1)).await,
        "after 1: pending(eval:E1)"
    );
    assert_eq!(
        waiting_state(&case, e1).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::CacheStorageFull)
        ),
        "after 1: evaluation(E1)"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn waiting_refresh_failure_still_refreshes_dispatches_and_aborts_later_evaluations() {
    use sea_orm::ConnectionTrait;

    let (ex, ey, ez) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let unmet = WaitingReason::workers(
        vec![UnmetRequirement {
            architecture: ARM.into(),
            required_features: Vec::new(),
            build_count: 1,
        }],
        1,
        vec![X86.into()],
    )
    .to_json();
    let case = SchedulerCase::start(
        shape()
            .evalq(ex)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json())
            .one(ey, A, A_OUT)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::graph_stuck(1).to_json())
            .one(ez, B, B_OUT)
            .system(ARM)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(unmet),
    )
    .await;
    case.connect(W1, w_all()).await;
    case.backdate_evaluation(ez, ago(301)).await;

    let db = case.db.connection();
    db.execute_unprepared(&format!(
        "CREATE FUNCTION fail_evaluation_x_status_change() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.id = '{ex}' AND NEW.status IS DISTINCT FROM OLD.status THEN \
         RAISE EXCEPTION 'refused'; END IF; RETURN NEW; END $$; \
         CREATE TRIGGER fail_evaluation_x_status_change BEFORE UPDATE ON evaluation \
         FOR EACH ROW EXECUTE FUNCTION fail_evaluation_x_status_change();"
    ))
    .await
    .expect("the trigger installs");

    case.build_assign_pass(true, true).await;
    assert_eq!(
        case.evaluation(ex).await.status,
        EvaluationStatus::Waiting,
        "after 1: status of evaluation(EX) did not change"
    );
    assert_eq!(
        waiting_state(&case, ey).await,
        (EvaluationStatus::Building, None),
        "after 1: the same pass refreshes evaluation(EY)"
    );
    assert_eq!(
        case.evaluation(ez).await.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(EZ) no worker can build is aborted in the same pass"
    );
    let assignment_for_w1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 acquired a build job");
    assert_eq!(
        assignment_for_w1.job_id(),
        case.key(A).await,
        "after 1: build:A of EY is the build job from the request_job of W1"
    );

    db.execute_unprepared(
        "DROP TRIGGER fail_evaluation_x_status_change ON evaluation; \
         DROP FUNCTION fail_evaluation_x_status_change();",
    )
    .await
    .expect("the trigger drops");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn row_lock_wait_timeout_does_not_stop_pass_from_refreshing_and_dispatching_later_evaluation()
{
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use std::time::{Duration, Instant};

    let (ex, ey) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evalq(ex)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json())
            .one(ey, A, A_OUT)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::graph_stuck(1).to_json()),
    )
    .await;
    case.connect(W1, w_all()).await;

    let evaluation_ex_row_lock_transaction = case
        .db
        .connection()
        .begin()
        .await
        .expect("start transaction for row lock");
    evaluation_ex_row_lock_transaction
        .execute_unprepared(&format!(
            "SELECT 1 FROM evaluation WHERE id = '{ex}' FOR NO KEY UPDATE"
        ))
        .await
        .expect("Lock the row for evaluation(EX).");

    let started = Instant::now();
    let pass =
        tokio::time::timeout(Duration::from_secs(20), case.build_assign_pass(true, true)).await;
    let elapsed = started.elapsed();
    if pass.is_err() {
        evaluation_ex_row_lock_transaction
            .rollback()
            .await
            .expect("row lock released");
        panic!(
            "after 1: the build assign pass did not finish within 20 s while another transaction held the lock on the evaluation(EX) row."
        );
    }
    assert!(
        elapsed < Duration::from_secs(15),
        "after 1: build assign pass done within 15 s, elapsed {elapsed:?}"
    );
    assert_eq!(
        case.evaluation(ex).await.status,
        EvaluationStatus::Waiting,
        "after 1: status of evaluation(EX) did not change"
    );
    assert_eq!(
        waiting_state(&case, ey).await,
        (EvaluationStatus::Building, None),
        "after 1: the same pass refreshes evaluation(EY)"
    );
    let assignment_for_w1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 acquired a build job");
    assert_eq!(
        assignment_for_w1.job_id(),
        case.key(A).await,
        "after 1: build:A of EY is the build job from the request_job of W1"
    );

    evaluation_ex_row_lock_transaction
        .rollback()
        .await
        .expect("row lock released");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn test_refresh_locked_counter_delta_timeout_assign_pass_dispatches() {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use std::time::{Duration, Instant};

    let (ex, ey) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evalq(ex)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json())
            .one(ey, A, A_OUT)
            .evaluation_status(EvaluationStatus::Building),
    )
    .await;
    case.connect(W1, w_all()).await;

    let db = case.db.connection();
    db.execute_unprepared(&format!(
        "INSERT INTO evaluation_shared_build_delta \
         (evaluation, named, active, failed, queued, building) \
         VALUES ('{ex}', 0, 0, 0, 0, 0)"
    ))
    .await
    .expect("Insert counter delta row for evaluation(EX)");
    let evaluation_ex_counter_delta_row_lock_transaction =
        db.begin().await.expect("start transaction for row lock");
    evaluation_ex_counter_delta_row_lock_transaction
        .execute_unprepared(&format!(
            "SELECT 1 FROM evaluation_shared_build_delta WHERE evaluation = '{ex}' FOR UPDATE"
        ))
        .await
        .expect("Lock counter delta row for evaluation(EX)");

    let started = Instant::now();
    let pass =
        tokio::time::timeout(Duration::from_secs(20), case.build_assign_pass(true, true)).await;
    let elapsed = started.elapsed();
    if pass.is_err() {
        evaluation_ex_counter_delta_row_lock_transaction
            .rollback()
            .await
            .expect("row lock released");
        panic!(
            "after 1: the build assign pass did not finish within 20 s. The fold of the counter deltas waited on a row lock for that whole time."
        );
    }
    assert!(
        elapsed < Duration::from_secs(15),
        "after 1: build assign pass done within 15 s, elapsed {elapsed:?}"
    );
    let assignment_for_w1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets a build job even though the waiting refresh failed.");
    assert_eq!(
        assignment_for_w1.job_id(),
        case.key(A).await,
        "after 1: build:A of EY is the build job from the request_job of W1"
    );

    evaluation_ex_counter_delta_row_lock_transaction
        .rollback()
        .await
        .expect("row lock released");
    case.check("step 1").await;

    case.finish().await;
}

gradient_db::sql! {
    READ_STATEMENT_AND_LOCK_TIMEOUT_SETTINGS_QUERY = "SELECT current_setting('statement_timeout') AS statement_timeout, \
                 current_setting('lock_timeout') AS lock_timeout",
        params = [];
}

#[tokio::test]
async fn waiting_refresh_pool_connections_carry_statement_and_lock_timeouts() {
    use sea_orm::ConnectionTrait;

    async fn limits(db: &gradient_db::WorkerDb) -> (String, String) {
        let row = db
            .query_one_raw(READ_STATEMENT_AND_LOCK_TIMEOUT_SETTINGS_QUERY.stmt())
            .await
            .expect("read settings")
            .expect("settings query to return one row");
        (
            row.try_get("", "statement_timeout")
                .expect("reading statement_timeout"),
            row.try_get("", "lock_timeout")
                .expect("Reading lock_timeout"),
        )
    }

    let case = SchedulerCase::start(shape()).await;
    case.build_assign_pass(true, true).await;

    assert_eq!(
        limits(&case.scheduler.waiting_refresh_database_pool.worker_db).await,
        ("5s".to_owned(), "2s".to_owned()),
        "after 1: connections in the pool for the waiting refresh keep the time limits"
    );
    assert_eq!(
        limits(&case.scheduler.db.worker_db).await,
        ("0".to_owned(), "0".to_owned()),
        "after 1: shared pool connections keep the server defaults"
    );

    case.finish().await;
}

#[tokio::test]
async fn waiting_refresh_pass_that_uses_up_time_limit_skips_remaining_evaluations_and_next_pass_refreshes_skipped_first()
 {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use std::time::Duration;

    let (ex, ez, ey) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = SchedulerCase::start(
        shape()
            .evalq(ex)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::NoCache.to_json())
            .evalq(ez)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::Draining.to_json())
            .one(ey, A, A_OUT)
            .evaluation_status(EvaluationStatus::Building),
    )
    .await;
    case.connect(W1, w_all()).await;
    case.scheduler
        .waiting_refresh_state_between_passes
        .lock()
        .budget = Duration::from_secs(1);

    let evaluation_ex_row_lock_transaction = case
        .db
        .connection()
        .begin()
        .await
        .expect("start transaction for row lock");
    evaluation_ex_row_lock_transaction
        .execute_unprepared(&format!(
            "SELECT 1 FROM evaluation WHERE id = '{ex}' FOR NO KEY UPDATE"
        ))
        .await
        .expect("Lock the row for evaluation(EX).");

    for step in 1..=2 {
        let pass =
            tokio::time::timeout(Duration::from_secs(20), case.build_assign_pass(true, true)).await;
        if pass.is_err() {
            evaluation_ex_row_lock_transaction
                .rollback()
                .await
                .expect("row lock released");
            panic!("after {step}: the build assign pass did not finish within 20 s");
        }
        if step == 1 {
            assert_eq!(
                waiting_state(&case, ez).await,
                (EvaluationStatus::Waiting, Some(WaitingReason::Draining)),
                "after 1: evaluation(EZ) is skipped because the refresh of evaluation(EX) used up the budget"
            );
            let assignment_for_w1 = case
                .request_job(W1, JobKind::Build)
                .await
                .expect("after 1: W1 gets a build job even though refresh stopped early.");
            assert_eq!(
                assignment_for_w1.job_id(),
                case.key(A).await,
                "after 1: build:A of EY is the build job from the request_job of W1"
            );
        }
    }
    assert_ne!(
        waiting_state(&case, ez).await,
        (EvaluationStatus::Waiting, Some(WaitingReason::Draining)),
        "after 2: pass 2 did the first refresh of evaluation(EZ), before any other evaluation. Pass 1 skipped that refresh."
    );

    evaluation_ex_row_lock_transaction
        .rollback()
        .await
        .expect("row lock released");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn unstick_after_waiting_refresh_budget_keeps_waiting_reason() {
    use crate::waiting_state::tests::TestLogEventRecorder;
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use std::time::Duration;

    const PRODUCTION_UNSTICK_INFO_LOG_MESSAGE: &str = "graph stuck: pool can build every pending shared build but none is dispatchable; self-healing";

    let (ez, ey) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evaluation(ez)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::workers(Vec::new(), 1, vec![X86.into()]).to_json())
            .derivation(A)
            .walked()
            .probed()
            .entry_point(&attr_of(A))
            .job()
            .output(A_OUT)
            .one(ey, B, B_OUT)
            .evaluation_status(EvaluationStatus::Building),
    )
    .await;
    case.connect(W1, w_all()).await;
    case.resync_startable_set().await;
    let events = TestLogEventRecorder::default();
    let _capture = tracing::subscriber::set_default(events.clone());

    case.scheduler
        .waiting_refresh_state_between_passes
        .lock()
        .budget = Duration::from_millis(300);
    let derivation_feature_lock_transaction = case
        .db
        .connection()
        .begin()
        .await
        .expect("start transaction to lock the table");
    derivation_feature_lock_transaction
        .execute_unprepared("LOCK TABLE derivation_feature IN ACCESS EXCLUSIVE MODE")
        .await
        .expect("derivation_feature table has a lock");
    let release = case.shutdown.spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        derivation_feature_lock_transaction
            .rollback()
            .await
            .expect("table lock released");
    });

    let pass = tokio::time::timeout(Duration::from_secs(20), case.refresh_waiting_state()).await;
    release
        .await
        .expect("task to release the table lock finished");
    assert!(
        pass.is_ok(),
        "after 1: waiting refresh must end within 20 seconds"
    );
    let (status, reason) = waiting_state(&case, ez).await;
    assert!(
        status == EvaluationStatus::Waiting
            && matches!(&reason, Some(WaitingReason::Workers { unmet, .. }) if unmet.is_empty()),
        "after 1: waiting reason of evaluation(EZ) is 'workers', with nothing unmet and no graph stuck: {status:?} {reason:?}"
    );
    assert_eq!(
        events.count(tracing::Level::INFO, PRODUCTION_UNSTICK_INFO_LOG_MESSAGE),
        0,
        "after 1: The spent budget delayed the unstick for evaluation(EZ). The unstick doesn't reach the graph writer."
    );
    let assignment_for_w1 = case.request_job(W1, JobKind::Build).await.expect(
        "after 1: W1 got a build job although the scheduler put unstick off to the next pass",
    );
    assert_eq!(
        assignment_for_w1.job_id(),
        case.key(B).await,
        "after 1: the build job that request_job of W1 returns is build:B of EY"
    );
    case.check("step 1").await;

    case.scheduler
        .waiting_refresh_state_between_passes
        .lock()
        .budget = crate::loops::WAITING_REFRESH_TIME_BUDGET;
    case.build_assign_pass(true, true).await;
    assert_eq!(
        waiting_state(&case, ez).await,
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::graph_stuck(1)),
        ),
        "after 2: evaluation(EZ) got unstuck in a pass with budget left"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Created,
        "after 2: sb(A).status"
    );
    assert_eq!(
        events.count(tracing::Level::INFO, PRODUCTION_UNSTICK_INFO_LOG_MESSAGE),
        1,
        "after 2: exactly one self-healing message for evaluation(EZ) reached the graph writer task"
    );
    case.check("step 2").await;

    case.finish().await;
}
