/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::exec_jobs::run_again::RunRefusal;
use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::{self, DispatchedJobKind};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::{self, ExecJobStatus};
use gradient_entity::exec_job_attempt;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::exec_job_continuation::{self, ContinuationStatus};
use gradient_entity::ids::{DispatchedJobId, EvaluationId, ProjectId};
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_types::WaitingReason;
use gradient_types::exec::{DISABLED_FOR_TASK, ExecInventory, ExecSpec, Phase, SopsFile};
use gradient_wire::types::BuildFailureKind;
use gradient_wire::types::{DiscoveredDerivation, FlakeSource, Job, JobKind};

use super::{
    A, A_OUT, B, B_OUT, CaseShapes, SchedulerCase, drv, eval_key, failure, w_all, w_exec,
    with_attr, with_output,
};
use crate::jobs::Assignment;
use crate::test_jobs::allow_all;

pub(super) const W1: &str = "w1";
pub(super) const W2: &str = "w2";
pub(super) const ATTR: &str = "apps.x86_64-linux.a";

pub(super) fn exec_shape(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(ATTR)
        .job()
        .output(A_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(A)
}

pub(super) fn spec() -> ExecSpec {
    ExecSpec {
        program: format!("/nix/store/{A_OUT}-a/bin/a"),
        nodes: 1,
        ..ExecSpec::default()
    }
}

pub(super) fn inventory() -> ExecInventory {
    ExecInventory {
        labels: vec![W1.into()],
        allow: allow_all(),
        ..ExecInventory::default()
    }
}

async fn seed_queued_job(case: &SchedulerCase, spec: &ExecSpec) {
    let retries = spec.retries;
    let spec = serde_json::to_string(spec).expect("the spec encodes");
    case.db
        .execute("UPDATE project SET exec_enabled = true")
        .await;
    case.db.execute("UPDATE task SET exec_enabled = true").await;
    case.db
        .execute(&format!(
            "UPDATE entry_point SET exec = '{spec}'::jsonb WHERE eval = '{ATTR}'"
        ))
        .await;
    case.db
        .execute(&format!(
            "INSERT INTO exec_job (id, evaluation, entry_point, program_derivation, requirement, \
             spec, retries_left, status, created_at) \
             SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, '{{}}'::jsonb, e.exec, \
             {retries}, 0, now() AT TIME ZONE 'UTC' FROM entry_point e WHERE e.exec IS NOT NULL"
        ))
        .await;
}

async fn with_queued_job(evaluation: EvaluationId, spec: ExecSpec) -> SchedulerCase {
    SchedulerCase::start_then(exec_shape(evaluation), async |case| {
        seed_queued_job(case, &spec).await;
    })
    .await
}

async fn dispatched(case: &SchedulerCase) -> (String, DispatchedJobId) {
    case.connect(W1, w_exec(inventory()), &[]).await;
    case.assign_exec_jobs().await;
    let assignment = case
        .request_job(W1, JobKind::Exec)
        .await
        .expect("an exec job");
    (assignment.job_id().to_owned(), assignment.assignment_id())
}

async fn the_attempt(case: &SchedulerCase) -> exec_job_attempt::Model {
    let mut attempts = case.db.rows::<exec_job_attempt::Entity>().await;
    assert_eq!(attempts.len(), 1, "one attempt");
    attempts.remove(0)
}

async fn the_job(case: &SchedulerCase) -> exec_job::Model {
    let mut jobs = case.db.rows::<exec_job::Entity>().await;
    assert_eq!(jobs.len(), 1, "one exec job");
    jobs.remove(0)
}

#[tokio::test]
async fn a_queued_exec_job_reaches_a_matching_exec_worker() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(e1, spec()).await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.assign_exec_jobs().await;
    let assignment = case
        .request_job(W1, JobKind::Exec)
        .await
        .expect("after 1: an exec job");

    assert!(
        matches!(assignment.job, Job::Exec(_)),
        "after 1: the job kind"
    );
    let job = the_job(&case).await;
    assert_eq!(job.status, ExecJobStatus::Starting, "after 1: the job");
    let attempts = case.db.rows::<exec_job_attempt::Entity>().await;
    assert_eq!(
        attempts
            .iter()
            .map(|a| (a.number, a.worker.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, W1)],
        "after 1: the attempts"
    );
    let rows = case.db.rows::<dispatched_job::Entity>().await;
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![DispatchedJobKind::Exec],
        "after 1: the dispatched rows"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_exec_offer_carries_the_program_closure_with_its_sizes() {
    let e1 = EvaluationId::now_v7();
    let shape = exec_shape(e1)
        .derivation(B)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(B_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(B)
        .derivation(A)
        .runtime_depends_on(B);
    let case = SchedulerCase::start_then(shape, async |case| {
        seed_queued_job(case, &spec()).await;
    })
    .await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.assign_exec_jobs().await;
    let offer = case.offers_full(W1).await;

    let mut paths: Vec<(String, Option<u64>)> = offer
        .candidates
        .iter()
        .flat_map(|candidate| &candidate.required_paths)
        .map(|path| {
            let size = path.cache_info.as_ref().map(|info| info.nar_size);
            (path.path.clone(), size)
        })
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            (format!("/nix/store/{A_OUT}-a"), Some(5)),
            (format!("/nix/store/{B_OUT}-b"), Some(5)),
        ],
        "after 1: the program and its runtime dependency with their sizes"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_unplaceable_job_stores_its_wait_reason() {
    let e1 = EvaluationId::now_v7();
    let printer = ExecSpec {
        labels: vec!["3d-printer".into()],
        ..spec()
    };
    let case = with_queued_job(e1, printer).await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.assign_exec_jobs().await;

    let reason = the_job(&case)
        .await
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);
    assert!(
        matches!(
            reason,
            Some(WaitingReason::ExecWorkers {
                matching_workers: 0,
                ..
            })
        ),
        "after 1: the wait reason: {reason:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_lost_worker_requeues_its_exec_job_without_a_retry() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(
        e1,
        ExecSpec {
            retries: 2,
            ..spec()
        },
    )
    .await;
    dispatched(&case).await;

    case.disconnect(W1).await;

    let job = the_job(&case).await;
    assert_eq!(
        (job.status, job.retries_left),
        (ExecJobStatus::Queued, 2),
        "after 1: re-queued with its retries"
    );
    assert_eq!(
        the_attempt(&case).await.end_reason,
        Some(AttemptEndReason::Lost),
        "after 1: the attempt"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_non_zero_exit_reported_by_the_worker_restarts_the_job() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(
        e1,
        ExecSpec {
            retries: 1,
            ..spec()
        },
    )
    .await;
    let (key, assignment) = dispatched(&case).await;

    case.fail(W1, &key, assignment, failure(BuildFailureKind::Permanent))
        .await;

    let job = the_job(&case).await;
    assert_eq!(
        (job.status, job.retries_left),
        (ExecJobStatus::Queued, 0),
        "after 1: re-queued with the retry spent"
    );
    assert_eq!(
        the_attempt(&case).await.end_reason,
        Some(AttemptEndReason::Failed),
        "after 1: the attempt"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_abort_sends_abort_job_and_ends_aborted() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(e1, spec()).await;
    let (key, _) = dispatched(&case).await;
    case.signals(W1);

    let aborted = case
        .scheduler
        .abort_exec_job(the_job(&case).await.id)
        .await
        .expect("the abort runs");

    assert!(aborted, "after 1: the job was open");
    assert_eq!(
        the_job(&case).await.status,
        ExecJobStatus::Aborted,
        "after 1: the job"
    );
    assert!(
        case.signals(W1)
            .iter()
            .any(|s| matches!(s, SessionSignal::Abort { job_id, .. } if *job_id == key)),
        "after 1: the worker was told to abort {key}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_refused_job_runs_again_once_its_task_switch_is_on() {
    let e1 = EvaluationId::now_v7();
    let retried = ExecSpec {
        retries: 2,
        ..spec()
    };
    let case = with_queued_job(e1, retried).await;
    case.db
        .execute("UPDATE task SET exec_enabled = false")
        .await;
    case.assign_exec_jobs().await;
    let job = the_job(&case).await;
    assert_eq!(
        (job.status, job.message.as_deref()),
        (ExecJobStatus::Failed, Some(DISABLED_FOR_TASK)),
        "after 1: refused"
    );

    let still_off = case.scheduler.run_exec_job(job.id).await.expect("the run");
    assert_eq!(
        still_off,
        Err(RunRefusal::Disabled(DISABLED_FOR_TASK)),
        "after 2: the switch is named"
    );
    assert_eq!(
        the_job(&case).await.status,
        ExecJobStatus::Failed,
        "after 2: the job"
    );

    case.db.execute("UPDATE task SET exec_enabled = true").await;
    let ran = case.scheduler.run_exec_job(job.id).await.expect("the run");
    assert_eq!(ran, Ok(()), "after 3: accepted");
    let queued = the_job(&case).await;
    assert_eq!(
        (
            queued.status,
            queued.message,
            queued.finished_at,
            queued.retries_left
        ),
        (ExecJobStatus::Queued, None, None, 2),
        "after 3: queued like a new job"
    );

    dispatched(&case).await;
    assert_eq!(the_attempt(&case).await.number, 1, "after 4: the attempt");
    case.finish().await;
}

#[tokio::test]
async fn a_failed_job_runs_again_as_its_next_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(e1, spec()).await;
    let (key, assignment) = dispatched(&case).await;
    let job = the_job(&case).await;
    let open = case.scheduler.run_exec_job(job.id).await.expect("the run");
    assert_eq!(open, Err(RunRefusal::Open), "after 1: an open job");

    case.fail(W1, &key, assignment, failure(BuildFailureKind::Permanent))
        .await;
    assert_eq!(
        the_job(&case).await.status,
        ExecJobStatus::Failed,
        "after 2: no retry left"
    );

    let ran = case.scheduler.run_exec_job(job.id).await.expect("the run");
    assert_eq!(ran, Ok(()), "after 3: accepted");
    case.assign_exec_jobs().await;
    case.request_job(W1, JobKind::Exec)
        .await
        .expect("after 4: the job again");
    let mut attempts = case.db.rows::<exec_job_attempt::Entity>().await;
    attempts.sort_by_key(|attempt| attempt.number);
    assert_eq!(
        attempts
            .iter()
            .map(|attempt| (attempt.number, attempt.end_reason))
            .collect::<Vec<_>>(),
        vec![(1, Some(AttemptEndReason::Failed)), (2, None)],
        "after 4: the first attempt stays as history"
    );
    case.finish().await;
}

const EVALUATOR: &str = "we";
const SECOND_ATTR: &str = "apps.x86_64-linux.b";

fn pre_warms(signals: &[SessionSignal]) -> Vec<u32> {
    signals
        .iter()
        .filter_map(|signal| match signal {
            SessionSignal::PreWarmVms { count } => Some(*count),
            _ => None,
        })
        .collect()
}

fn exec_root() -> DiscoveredDerivation {
    DiscoveredDerivation {
        exec: Some(spec()),
        ..with_attr(with_output(drv(A, &[]), A_OUT), ATTR)
    }
}

fn queued_evaluation_with_a_cached_program(evaluation: EvaluationId) -> Shape {
    shape()
        .evalq(evaluation)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(A)
}

async fn exec_recorded_mid_stream(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> (String, DispatchedJobId) {
    case.db
        .execute("UPDATE project SET exec_enabled = true")
        .await;
    case.db.execute("UPDATE task SET exec_enabled = true").await;
    case.connect(W1, w_exec(inventory()), &[]).await;
    case.connect(EVALUATOR, w_all(), &[]).await;
    let assignment = case.evalrun(EVALUATOR, evaluation).await;
    let key = eval_key(evaluation);
    case.advance_evaluation(EVALUATOR, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.signals(W1);
    case.report_eval_result(EVALUATOR, &key, vec![exec_root()])
        .await;
    (key, assignment)
}

#[tokio::test]
async fn an_exec_entry_point_recorded_mid_stream_sends_pre_warm_vms() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_evaluation_with_a_cached_program(e1)).await;

    exec_recorded_mid_stream(&case, e1).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 1: W1");
    case.finish().await;
}

#[tokio::test]
async fn a_job_waiting_for_its_build_counts_and_dispatch_drops_it() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_evaluation_with_a_cached_program(e1)).await;
    let (key, assignment) = exec_recorded_mid_stream(&case, e1).await;
    case.signals(W1);

    case.complete(EVALUATOR, &key, assignment, Vec::new(), 0)
        .await;
    case.settled().await;
    assert_eq!(
        the_job(&case).await.status,
        ExecJobStatus::Created,
        "after 1: the job waits for its build"
    );
    case.scheduler.refresh_pre_warm().await;
    assert!(
        pre_warms(&case.signals(W1)).is_empty(),
        "after 1: the count stays 1"
    );

    case.assign_exec_jobs().await;
    case.request_job(W1, JobKind::Exec)
        .await
        .expect("after 2: W1 gets the exec job");
    case.assign_exec_jobs().await;

    assert_eq!(
        pre_warms(&case.signals(W1)).last(),
        Some(&0),
        "after 2: the dispatched job counts nowhere"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_aborted_evaluation_drops_its_exec_jobs() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_evaluation_with_a_cached_program(e1)).await;
    exec_recorded_mid_stream(&case, e1).await;
    case.signals(W1);

    case.abort_evaluation(e1).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![0], "after 1: W1");
    case.finish().await;
}

#[tokio::test]
async fn a_custom_image_or_finished_job_counts_nowhere() {
    let e1 = EvaluationId::now_v7();
    let spec = serde_json::to_string(&spec()).expect("the spec encodes");
    let shape = exec_shape(e1).entry_point(SECOND_ATTR);
    let case = SchedulerCase::start_then(shape, async |case| {
        case.db
            .execute(&format!("UPDATE entry_point SET exec = '{spec}'::jsonb"))
            .await;
        case.db
            .execute(&format!(
                "INSERT INTO exec_job (id, evaluation, entry_point, program_derivation, \
                 image_derivation, requirement, spec, retries_left, status, created_at) \
                 SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, \
                 CASE WHEN e.eval = '{ATTR}' THEN e.derivation END, '{{}}'::jsonb, e.exec, 0, \
                 CASE WHEN e.eval = '{ATTR}' THEN 0 ELSE 3 END, now() AT TIME ZONE 'UTC' \
                 FROM entry_point e"
            ))
            .await;
    })
    .await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.scheduler.refresh_pre_warm().await;

    assert!(
        pre_warms(&case.signals(W1)).is_empty(),
        "after 1: no count for W1"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_reconnected_worker_gets_its_count_again() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(e1, spec()).await;
    case.connect(W1, w_exec(inventory()), &[]).await;
    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 1: W1");

    case.disconnect(W1).await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 2: W1");
    case.finish().await;
}

#[tokio::test]
async fn a_worker_of_another_project_gets_no_count() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(e1, spec()).await;

    case.connect(W2, w_exec(inventory()), &[ProjectId::now_v7()])
        .await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 1: W1");
    assert!(
        pre_warms(&case.signals(W2)).is_empty(),
        "after 1: no count for W2"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_wrong_sops_recipient_fails_the_job_before_dispatch() {
    let e1 = EvaluationId::now_v7();
    let spec = ExecSpec {
        secrets: vec!["wandb-token".into()],
        sops: vec![SopsFile {
            path: "secrets/train.yaml".into(),
            ciphertext: include_bytes!("../../../gradient-sops/tests/fixtures/other.yaml").to_vec(),
        }],
        ..spec()
    };
    let case = SchedulerCase::start_then(exec_shape(e1), async |case| {
        let (private_key, public_key) =
            gradient_sources::generate_ssh_key(&case.ctx.config.secrets.crypt_file)
                .expect("a project key");
        case.db
            .execute(&format!(
                "UPDATE project SET private_key = '{private_key}', public_key = '{public_key}'"
            ))
            .await;
        seed_queued_job(case, &spec).await;
    })
    .await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.assign_exec_jobs().await;

    let job = the_job(&case).await;
    assert_eq!(
        (job.status, job.message.as_deref()),
        (
            ExecJobStatus::Failed,
            Some("sops file secrets/train.yaml: not encrypted for the project key")
        ),
        "after 1: the job"
    );
    assert!(
        case.db.rows::<dispatched_job::Entity>().await.is_empty(),
        "after 1: no dispatch"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_requeued_job_carries_its_last_phase_to_the_next_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_job(e1, spec()).await;
    let (key, _) = dispatched(&case).await;
    case.scheduler
        .handle_exec_phase(W1, &key, "epoch-3".into(), b"ckpt".to_vec())
        .await;

    case.disconnect(W1).await;
    case.connect(W2, w_exec(inventory()), &[]).await;
    case.assign_exec_jobs().await;
    let assignment = case
        .request_job(W2, JobKind::Exec)
        .await
        .expect("after 1: the job reaches W2");

    let Job::Exec(job) = assignment.job else {
        panic!("after 1: an exec job")
    };
    assert_eq!(
        (job.context.last_phase, job.attempt),
        (
            Some(Phase {
                name: "epoch-3".into(),
                payload: b"ckpt".to_vec()
            }),
            2
        ),
        "after 1: the second attempt starts from the phase"
    );
    case.finish().await;
}

const CONTINUED_ATTR: &str = "packages.x86_64-linux.b";

async fn with_queued_continuation(evaluation: EvaluationId) -> SchedulerCase {
    let case = with_queued_job(evaluation, spec()).await;
    case.db
        .execute(&format!(
            "INSERT INTO exec_job_continuation \
             (exec_job, evaluation, attributes, status, created_at, updated_at) \
             SELECT id, evaluation, '[\"{CONTINUED_ATTR}\"]'::jsonb, {queued}, \
             now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC' FROM exec_job",
            queued = i16::from(ContinuationStatus::Queued),
        ))
        .await;
    case
}

async fn dispatched_continuation(case: &SchedulerCase) -> Assignment {
    case.connect(W1, w_all(), &[]).await;
    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.request_job(W1, JobKind::Flake)
        .await
        .expect("the continuation eval job")
}

#[tokio::test]
async fn a_queued_continuation_is_dispatched_as_an_eval_job_of_its_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_continuation(e1).await;

    let assignment = dispatched_continuation(&case).await;

    let exec_job = the_job(&case).await.id;
    let Job::Flake(flake) = &assignment.job else {
        panic!("after 1: a flake job")
    };
    assert_eq!(
        flake.wildcards,
        vec![CONTINUED_ATTR.to_owned()],
        "after 1: the wildcards"
    );
    let source = case
        .db
        .texts("SELECT COALESCE(flake_source, repository) FROM evaluation")
        .await;
    assert!(
        matches!(&flake.source, FlakeSource::Cached { store_path } if [store_path.clone()] == source.as_slice()),
        "after 1: the source {:?} is the evaluation's source {source:?}",
        flake.source
    );
    let rows = case.db.rows::<dispatched_job::Entity>().await;
    assert_eq!(
        rows.iter()
            .map(|r| (r.job_id.clone(), r.evaluation_id))
            .collect::<Vec<_>>(),
        vec![(Some(format!("continue:{exec_job}")), e1)],
        "after 1: the dispatched row"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_finished_continuation_job_ends_the_continuation_not_the_stream() {
    let e1 = EvaluationId::now_v7();
    let case = with_queued_continuation(e1).await;
    let assignment = dispatched_continuation(&case).await;
    let key = assignment.job_id().to_owned();
    case.report_eval_result(
        W1,
        &key,
        vec![with_attr(with_output(drv(B, &[]), B_OUT), CONTINUED_ATTR)],
    )
    .await;

    case.complete(W1, &key, assignment.assignment_id(), Vec::new(), 0)
        .await;

    let continuations = case.db.rows::<exec_job_continuation::Entity>().await;
    assert_eq!(
        continuations.iter().map(|c| c.status).collect::<Vec<_>>(),
        vec![ContinuationStatus::Done],
        "after 1: the continuation"
    );
    assert_eq!(
        case.db
            .rows::<gradient_entity::evaluation::Entity>()
            .await
            .iter()
            .map(|e| e.status)
            .collect::<Vec<_>>(),
        vec![EvaluationStatus::Building],
        "after 1: build b keeps the evaluation building"
    );
    case.finish().await;
}
