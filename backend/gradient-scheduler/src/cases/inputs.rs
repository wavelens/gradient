/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{EvaluationId, ProjectId};
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_types::InputError;
use gradient_wire::types::{InputOutcome, JobKind};

use super::{
    A, A_OUT, CaseShapes, Profile, SchedulerCase, eval_key, output_nar,
    remove_jobs_available_for_requested_kinds_signals, w_all, w_network_eval, w_network_secrets,
};

const W1: &str = "w1";
const W2: &str = "w2";
const W3: &str = "w3";
const W4: &str = "w4";

const SSH_REQUEST: &str = r#"{"builtin":"fetchTree","args":{"rev":"abc","type":"git","url":"git+ssh://git@example.invalid/input.git"},"storePath":"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input"}"#;
const SSH_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH: &str = r#"{"builtin":"fetchTree","args":{"ref":"main","type":"git","url":"git+ssh://git@example.invalid/input.git"}}"#;
const HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH: &str = r#"{"builtin":"fetchTree","args":{"ref":"main","type":"git","url":"git+https://example.invalid/input.git"}}"#;
const HTTPS_GIT_FETCH_TREE_REQUEST_WITH_STORE_PATH: &str = r#"{"builtin":"fetchTree","args":{"rev":"abc","type":"git","url":"git+https://example.invalid/input.git"},"storePath":"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input"}"#;
const HTTPS_FETCH_TARBALL_REQUEST_WITHOUT_HASH: &str =
    r#"{"builtin":"fetchTarball","args":{"url":"https://example.invalid/input.tar.gz"}}"#;
const FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH: &str = r#"{"builtin":"fetchTree","args":{"narHash":"sha256-OXhFkURii0pxIkZ2DeaOUN+tybxn+ccJvfDKIsA2UXk=","owner":"o","repo":"r","type":"github"}}"#;
const FETCHED: &str = r#"{"storePath":"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-input"}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fetch {
    worker: String,
    fetch_id: String,
    project: ProjectId,
    request: String,
    needs_secrets: bool,
}

#[tokio::test]
async fn i01_an_input_request_goes_to_a_network_and_secrets_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, super::w_network()).await;
    case.connect(W3, w_fetch()).await;

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
            needs_secrets: true,
        }],
        "after 1: only W3 gets FetchInput"
    );
    assert_eq!(case.input_waits(), 1, "after 1: one input wait");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn public_https_git_input_fetch_signal_has_needs_secrets_false_for_network_only_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    let fetches = fetches_of(&case, &[W1]);
    assert_eq!(
        fetches,
        vec![Fetch {
            worker: W1.into(),
            fetch_id: fetches[0].fetch_id.clone(),
            project: case.db.project_of(e1).await,
            request: HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH.into(),
            needs_secrets: false,
        }],
        "after 1: W1 with no secrets can fetch the public input"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn https_git_input_with_stored_https_credential_is_assigned_only_to_secrets_capable_worker_and_marked_needs_secrets()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let project = case.db.project_of(e1).await;
    evaluating(&case, W1, e1).await;
    gradient_db::project_git_https_credentials::upsert(
        &case.db.connection(),
        project,
        "example.invalid",
        "x-access-token",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();
    case.connect(W2, super::w_network()).await;
    case.connect(W3, w_fetch()).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    let fetches = fetches_of(&case, &[W1, W2, W3]);
    assert_eq!(
        fetches,
        vec![Fetch {
            worker: W3.into(),
            fetch_id: fetches[0].fetch_id.clone(),
            project,
            request: HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH.into(),
            needs_secrets: true,
        }],
        "after 1: only the secrets worker W3 gets FetchInput"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn i02_two_evaluations_share_one_fetch() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(same_project(e1, e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch()).await;

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
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 2: signals(W1)"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
        vec![result(e2, "r2", fetched())],
        "after 2: signals(W2)"
    );
    assert_eq!(case.input_waits(), 0, "after 2: no input wait left");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn two_projects_fetching_one_unpinned_ssh_input_get_separate_fetches() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch()).await;
    case.connect(W4, w_fetch()).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        SSH_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    case.input_request(
        W2,
        &eval_key(e2),
        "r2",
        SSH_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    let mut projects = fetches_of(&case, &[W1, W2, W3, W4])
        .into_iter()
        .map(|fetch| fetch.project)
        .collect::<Vec<_>>();
    let mut expected = vec![case.db.project_of(e1).await, case.db.project_of(e2).await];
    projects.sort();
    expected.sort();
    assert_eq!(projects, expected, "after 1: one FetchInput per project");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn two_projects_fetching_one_unpinned_https_tarball_get_separate_fetches_and_answers() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    evaluating_only_for_its_project(&case, W1, e1).await;
    evaluating_only_for_its_project(&case, W2, e2).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_FETCH_TARBALL_REQUEST_WITHOUT_HASH,
    )
    .await;
    case.input_request(
        W2,
        &eval_key(e2),
        "r2",
        HTTPS_FETCH_TARBALL_REQUEST_WITHOUT_HASH,
    )
    .await;
    let mut fetches = fetches_of(&case, &[W1, W2]);
    fetches.sort_by(|a, b| a.worker.cmp(&b.worker));
    assert_eq!(
        fetches
            .iter()
            .map(|fetch| (fetch.worker.as_str(), fetch.project))
            .collect::<Vec<_>>(),
        vec![(W1, p1), (W2, p2)],
        "after 1: one FetchInput per project, each to the worker of that project"
    );
    case.check("step 1").await;

    case.input_fetched(W1, &fetches[0].fetch_id, fetched())
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 2: signals(W1)"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
        vec![],
        "after 2: the answer for project 1 never reaches project 2"
    );
    assert_eq!(case.input_waits(), 1, "after 2: project 2 still waits");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn two_projects_fetching_one_content_pinned_input_get_separate_fetches_and_answer_reaches_only_first_project()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    evaluating_only_for_its_project(&case, W1, e1).await;
    evaluating_only_for_its_project(&case, W2, e2).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH,
    )
    .await;
    case.input_request(
        W2,
        &eval_key(e2),
        "r2",
        FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH,
    )
    .await;
    let mut fetches = fetches_of(&case, &[W1, W2]);
    fetches.sort_by(|a, b| a.worker.cmp(&b.worker));
    assert_eq!(
        fetches
            .iter()
            .map(|fetch| (fetch.worker.as_str(), fetch.project))
            .collect::<Vec<_>>(),
        vec![(W1, p1), (W2, p2)],
        "after 1: one FetchInput per project, each to the worker of that project"
    );
    case.check("step 1").await;

    let pinned =
        build_fetched_input_outcome_with_store_path(&content_pin_store_path_for_request_constant());
    case.input_fetched(W1, &fetches[0].fetch_id, pinned.clone())
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", pinned)],
        "after 2: signals(W1)"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
        vec![],
        "after 2: the answer for project 1 never reaches project 2"
    );
    assert_eq!(case.input_waits(), 1, "after 2: project 2 still waits");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn two_projects_fetching_one_pinned_https_git_input_with_stored_credential_in_one_project_get_separate_fetches()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    let (credentialed, public) = (case.db.project_of(e1).await, case.db.project_of(e2).await);
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    gradient_db::project_git_https_credentials::upsert(
        &case.db.connection(),
        credentialed,
        "example.invalid",
        "x-access-token",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();
    case.connect(W3, w_fetch()).await;
    case.connect(W4, w_fetch()).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITH_STORE_PATH,
    )
    .await;
    case.input_request(
        W2,
        &eval_key(e2),
        "r2",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITH_STORE_PATH,
    )
    .await;
    let mut fetches = fetches_of(&case, &[W1, W2, W3, W4])
        .into_iter()
        .map(|fetch| (fetch.project, fetch.needs_secrets))
        .collect::<Vec<_>>();
    let mut expected = vec![(credentialed, true), (public, false)];
    fetches.sort();
    expected.sort();
    assert_eq!(fetches, expected, "after 1: one FetchInput per project");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn i03_a_failed_fetch_answers_every_waiter() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(same_project(e1, e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch()).await;
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
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", failed.clone())],
        "after 2: signals(W1)"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
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
    case.connect(W2, w_fetch()).await;
    case.connect(W3, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(
        first.worker, W2,
        "after 1: the lower worker id wins the tie"
    );
    case.check("step 1").await;

    case.disconnect(W2).await;
    case.signals(W2);
    let second = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(second.worker, W3, "after 2: W3 takes over");
    assert_ne!(second.fetch_id, first.fetch_id, "after 2: a new fetch id");
    case.check("step 2").await;

    case.input_fetched(W2, &first.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![],
        "after 3: the late answer of W2 is ignored"
    );
    case.check("step 3").await;

    case.input_fetched(W3, &second.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
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

    case.connect(W2, w_fetch()).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        fetches_of(&case, &[W1, W2]),
        vec![],
        "after 3: no FetchInput for an aborted evaluation"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![],
        "after 3: no InputResult"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn i06_without_a_fetch_worker_the_evaluation_says_it_waits() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.refresh_waiting_state().await;
    case.build_assign_pass(false, false).await;
    let row = case.evaluation(e1).await;
    assert_eq!(
        (row.status, row.waiting_reason),
        (EvaluationStatus::EvaluatingDerivation, None),
        "after 1: evaluation(E1) keeps evaluating without a waiting reason"
    );
    assert_eq!(
        wait_messages(&case, e1).await,
        vec!["waiting for a worker with network and secrets to fetch an input".to_owned()],
        "after 1: one message names the missing worker, also after a second pass"
    );
    case.check("step 1").await;

    case.connect(W2, w_fetch()).await;
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
    case.connect(W1, w_all()).await;
    case.connect(W2, w_fetch()).await;
    case.admit_startable_moves().await;
    let build = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    case.signals(W1);
    case.check("step 1").await;

    case.input_request(W1, build.job_id(), "r1", SSH_REQUEST)
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![SessionSignal::InputResult {
            job_id: build.job_id().to_owned(),
            request_id: "r1".into(),
            outcome: InputOutcome::Failed {
                message: "job for this input request is not a running evaluation".into(),
            },
        }],
        "after 2: signals(W1) refuse the request"
    );
    assert_eq!(fetches_of(&case, &[W2]), vec![], "after 2: no fetch");
    assert_eq!(case.input_waits(), 0, "after 2: no wait opened");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn an_input_request_for_a_host_path_is_refused_without_a_fetch() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W3, w_fetch()).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        r#"{"builtin":"fetchTree","args":{"type":"path","path":"/run/credentials/x","narHash":"sha256-AAAA"}}"#,
    )
    .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(
            e1,
            "r1",
            InputOutcome::Failed {
                message: InputError::InputTypeNotAllowed("path".into()).to_string(),
            }
        )],
        "after 1: signals(W1) refuse the request"
    );
    assert_eq!(fetches_of(&case, &[W3]), vec![], "after 1: no FetchInput");
    assert_eq!(case.input_waits(), 0, "after 1: no wait opened");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn i08_fetch_progress_reaches_every_waiting_evaluation() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(same_project(e1, e2)).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    case.connect(W3, w_fetch()).await;
    case.connect(W4, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.input_request(W2, &eval_key(e2), "r2", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2, W3, W4]);
    case.check("step 1").await;

    case.input_fetch_progress(&first.worker, &first.fetch_id, 512, Some(1024))
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![progress(e1, "r1", 512, Some(1024))],
        "after 2: signals(W1)"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
        vec![progress(e2, "r2", 512, Some(1024))],
        "after 2: signals(W2)"
    );
    case.check("step 2").await;

    case.disconnect(&first.worker).await;
    case.signals(&first.worker);
    let second = single_fetch(&case, &[W1, W2, W3, W4]);
    case.input_fetch_progress(&first.worker, &first.fetch_id, 1024, Some(1024))
        .await;
    for worker in [W1, W2] {
        assert_eq!(
            remove_jobs_available_for_requested_kinds_signals(case.signals(worker)),
            vec![],
            "after 3: progress of the replaced fetch reaches no {worker}"
        );
    }
    case.input_fetch_progress(&second.worker, &second.fetch_id, 64, None)
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
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
    case.connect(W2, w_all()).await;
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
    case.connect(W2, w_all()).await;
    case.idle(W2, JobKind::Build).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let fetches = fetch_signals(&case, W2);
    let [SessionSignal::FetchInput { fetch_id, .. }] = fetches.as_slice() else {
        panic!("after 1: W2 gets one FetchInput, got {fetches:?}");
    };
    case.check("step 1").await;

    case.admit_startable_moves().await;
    assert_eq!(
        case.signals(W2),
        vec![],
        "After 2: fetch took W2's only build slot. No signal reached W2."
    );
    let taken = case.request_job(W2, JobKind::Build).await;
    assert!(
        taken.is_none(),
        "after 2: the fetch holds the only slot, got {:?}",
        taken.map(|t| t.job_id().to_owned())
    );
    case.check("step 2").await;

    case.input_fetched(W2, fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
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

#[tokio::test]
async fn i11_an_unplaced_fetch_goes_to_another_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.connect(W3, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(
        first.worker, W2,
        "after 1: the lower worker id wins the tie"
    );
    case.check("step 1").await;

    case.input_fetched(W2, &first.fetch_id, unplaced()).await;
    let second = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(second.worker, W3, "after 2: the fetch moves to W3");
    assert_ne!(second.fetch_id, first.fetch_id, "after 2: a new fetch id");
    case.check("step 2").await;

    case.input_fetched(W3, &second.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 3: the evaluation gets the result of W3"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn i12_an_unplaced_fetch_waits_when_no_other_worker_qualifies() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2]);
    case.check("step 1").await;

    case.input_fetched(W2, &first.fetch_id, unplaced()).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        fetches_of(&case, &[W1, W2]),
        vec![],
        "after 2: W2 is not asked again"
    );
    assert_eq!(case.input_waits(), 1, "after 2: the request still waits");
    assert_eq!(
        case.evaluation(e1).await.waiting_reason,
        None,
        "after 2: no waiting reason while evaluating"
    );
    assert_eq!(
        wait_messages(&case, e1).await,
        vec!["waiting for a worker with network and secrets to fetch an input".to_owned()],
        "after 2: the excluded W2 counts as no fetch worker"
    );
    case.check("step 2").await;

    case.connect(W3, w_fetch()).await;
    case.build_assign_pass(false, false).await;
    assert_eq!(
        single_fetch(&case, &[W1, W2, W3]).worker,
        W3,
        "after 3: W3 takes the fetch"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn pinned_input_wrong_store_path_is_not_passed_to_evaluation_worker_is_not_retried_and_another_worker_serves_it()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.connect(W3, w_fetch()).await;
    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH,
    )
    .await;
    let first = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(
        first.worker, W1,
        "after 1: the lowest network worker id wins"
    );
    case.check("step 1").await;

    case.input_fetched(
        W1,
        &first.fetch_id,
        build_fetched_input_outcome_with_store_path(
            "/nix/store/00000000000000000000000000000000-source",
        ),
    )
    .await;
    let second = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(second.worker, W2, "after 2: the fetch moves to W2, not W1");
    assert_eq!(case.input_waits(), 1, "after 2: the request still waits");
    case.check("step 2").await;

    let pinned =
        build_fetched_input_outcome_with_store_path(&content_pin_store_path_for_request_constant());
    case.input_fetched(W2, &second.fetch_id, pinned.clone())
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", pinned)],
        "after 3: only the pinned answer of W2 reaches the evaluation"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn pinned_input_answer_without_store_path_is_not_passed_to_evaluation_and_another_worker_serves_it()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH,
    )
    .await;
    let first = single_fetch(&case, &[W1, W2]);
    assert_eq!(
        first.worker, W1,
        "after 1: the lowest network worker id wins"
    );
    case.check("step 1").await;

    let without_store_path = InputOutcome::Fetched {
        response: serde_json::json!({ "narHash": "sha256-unused" }).to_string(),
    };
    case.input_fetched(W1, &first.fetch_id, without_store_path)
        .await;
    let second = single_fetch(&case, &[W1, W2]);
    assert_eq!(second.worker, W2, "after 2: the fetch moves to W2, not W1");
    assert_eq!(case.input_waits(), 1, "after 2: the request still waits");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn pinned_input_wrong_store_path_fails_waiting_evaluation_without_waiting_for_fetch_timeout()
{
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH,
    )
    .await;
    let fetch = single_fetch(&case, &[W1]);
    case.check("step 1").await;

    case.input_fetched(
        W1,
        &fetch.fetch_id,
        build_fetched_input_outcome_with_store_path(
            "/nix/store/00000000000000000000000000000000-source",
        ),
    )
    .await;
    let answers = remove_jobs_available_for_requested_kinds_signals(case.signals(W1));
    assert!(
        matches!(
            answers.as_slice(),
            [SessionSignal::InputResult {
                request_id,
                outcome: InputOutcome::Failed { message },
                ..
            }] if request_id == "r1" && message.contains(&content_pin_store_path_for_request_constant())
        ),
        "after 2: W1 gets a failure naming the pinned store path, got {answers:?}"
    );
    assert_eq!(case.input_waits(), 0, "after 2: no input wait left");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn pinned_input_fetch_with_pinned_store_path_reaches_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH,
    )
    .await;
    let fetch = single_fetch(&case, &[W1, W2]);

    let pinned =
        build_fetched_input_outcome_with_store_path(&content_pin_store_path_for_request_constant());
    case.input_fetched(&fetch.worker, &fetch.fetch_id, pinned.clone())
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", pinned)],
        "after 1: the evaluation gets the pinned answer"
    );
    assert_eq!(case.input_waits(), 0, "after 1: no input wait left");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn answer_from_worker_not_assigned_input_fetch_is_ignored_and_evaluation_keeps_waiting() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.connect(W3, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let fetch = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(fetch.worker, W2, "after 1: W2 gets the fetch");
    case.check("step 1").await;

    case.input_fetched(W3, &fetch.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![],
        "after 2: the answer of W3 never reaches the evaluation"
    );
    assert_eq!(
        fetches_of(&case, &[W1, W2, W3]),
        vec![],
        "after 2: the fetch stays with W2"
    );
    assert_eq!(case.input_waits(), 1, "after 2: the request still waits");
    case.check("step 2").await;

    case.input_fetched(W2, &fetch.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 3: the answer of W2 reaches the evaluation"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn input_fetch_timeout_moves_to_second_worker_and_drops_late_first_answer() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.connect(W3, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let first = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(first.worker, W2, "after 1: W2 gets the fetch");

    case.run_input_fetch_expiry_after(Duration::from_secs(3599))
        .await;
    assert_eq!(
        fetches_of(&case, &[W1, W2, W3]),
        vec![],
        "after 2: the fetch stays with W2 before the timeout"
    );
    case.check("step 2").await;

    case.run_input_fetch_expiry_after(Duration::from_secs(3600))
        .await;
    let second = single_fetch(&case, &[W1, W2, W3]);
    assert_eq!(second.worker, W3, "after 3: the fetch moves to W3");
    assert_ne!(second.fetch_id, first.fetch_id, "after 3: a new fetch id");
    case.check("step 3").await;

    case.input_fetched(W2, &first.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![],
        "after 4: the late answer of W2 is dropped"
    );
    assert_eq!(case.input_waits(), 1, "after 4: the request still waits");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn input_fetch_timeout_fails_waiting_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    single_fetch(&case, &[W1, W2]);

    case.run_input_fetch_expiry_after(Duration::from_secs(3600))
        .await;
    let answers = remove_jobs_available_for_requested_kinds_signals(case.signals(W1));
    assert!(
        matches!(
            answers.as_slice(),
            [SessionSignal::InputResult {
                request_id,
                outcome: InputOutcome::Failed { message },
                ..
            }] if request_id == "r1" && message.contains("3600 seconds")
        ),
        "after 1: W1 gets a failure, got {answers:?}"
    );
    assert_eq!(
        fetches_of(&case, &[W2]),
        vec![],
        "after 1: W2 is not asked again"
    );
    assert_eq!(case.input_waits(), 0, "after 1: no input wait left");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn input_fetch_timeout_fails_evaluation_waiting_for_input_no_connected_worker_can_fetch() {
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

    case.run_input_fetch_expiry_after(Duration::from_secs(3599))
        .await;
    assert_eq!(case.input_waits(), 1, "after 2: the request still waits");
    case.signals(W1);
    case.check("step 2").await;

    case.run_input_fetch_expiry_after(Duration::from_secs(3600))
        .await;
    let answers = remove_jobs_available_for_requested_kinds_signals(case.signals(W1));
    assert!(
        matches!(
            answers.as_slice(),
            [SessionSignal::InputResult {
                request_id,
                outcome: InputOutcome::Failed { message },
                ..
            }] if request_id == "r1"
                && message.contains("3600 seconds")
                && message.contains("git+ssh://git@example.invalid/input.git")
        ),
        "after 3: W1 gets a failure naming the input, got {answers:?}"
    );
    assert_eq!(case.input_waits(), 0, "after 3: no input wait left");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn i13_duplicate_input_request_keeps_one_waiting_request_and_answers_once() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let fetch = single_fetch(&case, &[W1, W2]);
    assert_eq!(case.input_waits(), 1, "after 1: one waiting request");
    case.check("step 1").await;

    case.input_fetched(W2, &fetch.fetch_id, fetched()).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 2: one answer to W1"
    );
    assert_eq!(case.input_waits(), 0, "after 2: no input wait left");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn evaluation_without_project_credentials_gets_failure_for_input_needing_project_ssh_key_without_fetch_or_wait()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).mark_not_allowed_project_credentials()).await;
    evaluating(&case, W1, e1).await;
    case.connect(W2, w_fetch()).await;

    case.input_request(W1, &eval_key(e1), "r1", SSH_REQUEST)
        .await;
    let answers = remove_jobs_available_for_requested_kinds_signals(case.signals(W1));
    assert!(
        matches!(
            answers.as_slice(),
            [SessionSignal::InputResult {
                request_id,
                outcome: InputOutcome::Failed { message },
                ..
            }] if request_id == "r1" && *message == crate::exec_secrets::SecretResolutionError::ProjectGitCredentialsWithheldForUnapprovedForkPullRequest.to_string()
        ),
        "after 1: W1 gets a refusal, got {answers:?}"
    );
    assert_eq!(fetches_of(&case, &[W2]), vec![], "after 1: no fetch");
    assert_eq!(case.input_waits(), 0, "after 1: no wait opened");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn unapproved_fork_pull_request_https_input_fetches_without_stored_credentials() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).mark_not_allowed_project_credentials()).await;
    let project = case.db.project_of(e1).await;
    evaluating(&case, W1, e1).await;
    store_example_invalid_https_git_credential_in_project(&case, project).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    let fetches = fetches_of(&case, &[W1]);
    assert_eq!(
        fetches,
        vec![Fetch {
            worker: W1.into(),
            fetch_id: fetches
                .first()
                .map(|f| f.fetch_id.clone())
                .unwrap_or_default(),
            project,
            request: HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH.into(),
            needs_secrets: false,
        }],
        "after 1: W1 without secrets fetches the input without the stored credential"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn test_unapproved_fork_credentialless_fetch_failure_sends_credentials_withheld_message() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).mark_not_allowed_project_credentials()).await;
    let project = case.db.project_of(e1).await;
    evaluating(&case, W1, e1).await;
    store_example_invalid_https_git_credential_in_project(&case, project).await;
    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    let fetch = single_fetch(&case, &[W1]);
    case.check("step 1").await;

    let failed = InputOutcome::Failed {
        message: "HTTP error 401".into(),
    };
    case.input_fetched(W1, &fetch.fetch_id, failed).await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(
            e1,
            "r1",
            build_failed_input_answer_with_withheld_credentials_error_and_worker_message(
                "HTTP error 401",
            ),
        )],
        "after 2: signals(W1)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn same_project_unapproved_credentialless_and_approved_credentialed_fetches_do_not_share_result()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case =
        SchedulerCase::start(same_project(e1, e2).mark_not_allowed_project_credentials()).await;
    let project = case.db.project_of(e1).await;
    evaluating(&case, W1, e1).await;
    evaluating(&case, W2, e2).await;
    store_example_invalid_https_git_credential_in_project(&case, project).await;
    case.connect(W3, w_fetch()).await;

    case.input_request(
        W1,
        &eval_key(e1),
        "r1",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    case.input_request(
        W2,
        &eval_key(e2),
        "r2",
        HTTPS_GIT_FETCH_TREE_REQUEST_WITHOUT_STORE_PATH,
    )
    .await;
    let fetches = fetches_of(&case, &[W1, W2, W3]);
    let mut needs_secrets = fetches.iter().map(|f| f.needs_secrets).collect::<Vec<_>>();
    needs_secrets.sort();
    assert_eq!(
        needs_secrets,
        vec![false, true],
        "after 1: one fetch with and one without credentials, got {fetches:?}"
    );
    case.check("step 1").await;

    let credentialed = fetches.iter().find(|f| f.needs_secrets).unwrap();
    case.input_fetched(&credentialed.worker, &credentialed.fetch_id, fetched())
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W1)),
        vec![result(e1, "r1", fetched())],
        "after 2: signals(W1)"
    );
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
        vec![],
        "after 2: signals(W2)"
    );
    case.check("step 2").await;

    let credentialless = fetches.iter().find(|f| !f.needs_secrets).unwrap();
    let failed = InputOutcome::Failed {
        message: "HTTP error 401".into(),
    };
    case.input_fetched(&credentialless.worker, &credentialless.fetch_id, failed)
        .await;
    assert_eq!(
        remove_jobs_available_for_requested_kinds_signals(case.signals(W2)),
        vec![result(
            e2,
            "r2",
            build_failed_input_answer_with_withheld_credentials_error_and_worker_message(
                "HTTP error 401",
            ),
        )],
        "after 3: signals(W2)"
    );
    assert_eq!(case.input_waits(), 0, "after 3: no input wait left");
    case.check("step 3").await;

    case.finish().await;
}

async fn store_example_invalid_https_git_credential_in_project(
    case: &SchedulerCase,
    project: ProjectId,
) {
    gradient_db::project_git_https_credentials::upsert(
        &case.db.connection(),
        project,
        "example.invalid",
        "x-access-token",
        "encrypted".into(),
        None,
    )
    .await
    .unwrap();
}

fn build_failed_input_answer_with_withheld_credentials_error_and_worker_message(
    worker_message: &str,
) -> InputOutcome {
    InputOutcome::Failed {
        message: format!(
            "{}\n{worker_message}",
            crate::exec_secrets::SecretResolutionError::ProjectGitCredentialsWithheldForUnapprovedForkPullRequest
        ),
    }
}

async fn wait_messages(case: &SchedulerCase, evaluation: EvaluationId) -> Vec<String> {
    case.messages(evaluation)
        .await
        .into_iter()
        .filter(|m| m.source.as_deref() == Some("fetch"))
        .map(|m| m.message)
        .collect()
}

fn unplaced() -> InputOutcome {
    InputOutcome::Unplaced {
        message: "no worker behind the shared worker can fetch the input".into(),
    }
}

async fn building_on_w2(case: &SchedulerCase) -> (String, gradient_entity::ids::DispatchedJobId) {
    let a = case.key(A).await;
    case.admit_startable_moves().await;
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

fn same_project(first: EvaluationId, second: EvaluationId) -> Shape {
    shape()
        .evalq(first)
        .evaluation(second)
        .same_task_as(first)
        .evaluation_status(EvaluationStatus::Queued)
        .branch("second")
}

async fn evaluating(case: &SchedulerCase, worker: &str, evaluation: EvaluationId) {
    case.connect(worker, w_network_eval()).await;
    run_evaluation(case, worker, evaluation).await;
}

async fn evaluating_only_for_its_project(
    case: &SchedulerCase,
    worker: &str,
    evaluation: EvaluationId,
) {
    let project = case.db.project_of(evaluation).await;
    case.connect_for_projects(worker, w_network_eval(), &[project])
        .await;
    run_evaluation(case, worker, evaluation).await;
}

async fn run_evaluation(case: &SchedulerCase, worker: &str, evaluation: EvaluationId) {
    let key = eval_key(evaluation);
    case.evalrun(worker, evaluation).await;
    case.advance_evaluation(worker, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.signals(worker);
    case.check("step 0").await;
}

fn fetches_of(case: &SchedulerCase, workers: &[&str]) -> Vec<Fetch> {
    let mut fetches = Vec::new();
    for worker in workers {
        for signal in remove_jobs_available_for_requested_kinds_signals(case.signals(worker)) {
            match signal {
                SessionSignal::FetchInput {
                    fetch_id,
                    project,
                    request,
                    needs_secrets,
                } => fetches.push(Fetch {
                    worker: (*worker).to_owned(),
                    fetch_id,
                    project,
                    request,
                    needs_secrets,
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

fn build_fetched_input_outcome_with_store_path(store_path: &str) -> InputOutcome {
    InputOutcome::Fetched {
        response: serde_json::json!({ "storePath": store_path }).to_string(),
    }
}

fn content_pin_store_path_for_request_constant() -> String {
    gradient_pool::InputRequest::parse(FETCH_TREE_GITHUB_REQUEST_JSON_WITH_NAR_HASH)
        .expect("the pinned test request parses")
        .content_hash_store_path
        .expect("the narHash pins the content")
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
