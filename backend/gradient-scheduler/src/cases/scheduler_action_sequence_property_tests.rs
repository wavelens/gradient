/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId};
use gradient_fixtures::shape::{Shape, shape};
use gradient_wire::types::{BuildFailureKind, JobKind};
use proptest::prelude::*;
use proptest::test_runner::{TestCaseError, TestRunner};

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, D, D_OUT, N, N_OUT, P, P_OUT, SchedulerCase, attr_of, failure,
    output_nar, w_all,
};

const SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES: [(&str, &str); 6] = [
    (A, A_OUT),
    (B, B_OUT),
    (C, C_OUT),
    (D, D_OUT),
    (N, N_OUT),
    (P, P_OUT),
];
const ACTION_WORKER_NAMES: [&str; 3] = ["w1", "w2", "w3"];
const WORKER_NAME_FOR_FINISHING_REMAINING_WORK: &str = "w9";

#[derive(Clone, Debug)]
struct GeneratedDerivationDependencyAndActionTestInput {
    derivation_dependency_indices: Vec<Vec<usize>>,
    actions_to_run: Vec<GeneratedSchedulerAction>,
}

#[derive(Clone, Copy, Debug)]
enum GeneratedSchedulerAction {
    WorkerAtSlotConnectsWithOneOrTwoBuildSlots { worker: usize, slots: u32 },
    WorkerDisconnected { worker: usize },
    WorkerAsksSchedulerForBuildJob { worker: usize },
    WorkerReportsBuildStarted { assignment: usize },
    UploadOutputAndReportBuildCompleted { assignment: usize },
    WorkerReportedBuildFailed { assignment: usize, permanent: bool },
    AdminAbortsBuildOfOneDerivation { derivation: usize },
    AdminAbortsWholeEvaluation,
    AdminRetriesDerivationBuild { derivation: usize },
    UpstreamCacheProbeNotFoundForEveryWaitingBuild,
    RequeueTransientlyFailedBuilds,
}

#[derive(Default)]
struct ConnectedWorkersAndJobAssignments {
    connected: [bool; ACTION_WORKER_NAMES.len()],
    assignments: Vec<WorkerJobAssignment>,
}

struct WorkerJobAssignment {
    worker: &'static str,
    derivation: usize,
    key: String,
    assignment: DispatchedJobId,
    worker_reported_build_started: bool,
}

fn derivation_dependency_lists_for_one_to_six_derivations() -> impl Strategy<Value = Vec<Vec<usize>>>
{
    (1..=SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES.len()).prop_flat_map(|count| {
        proptest::collection::vec(proptest::collection::vec(any::<bool>(), count), count).prop_map(
            move |edges| {
                edges
                    .iter()
                    .enumerate()
                    .map(|(from, row)| (from + 1..count).filter(|&to| row[to]).collect())
                    .collect()
            },
        )
    })
}

fn action_strategy() -> impl Strategy<Value = GeneratedSchedulerAction> {
    use GeneratedSchedulerAction::*;
    let worker = 0..ACTION_WORKER_NAMES.len();
    let assignment = 0_usize..8;
    let derivation = 0..SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES.len();
    prop_oneof![
        3 => (worker.clone(), 1_u32..=2)
            .prop_map(|(worker, slots)| WorkerAtSlotConnectsWithOneOrTwoBuildSlots { worker, slots }),
        1 => worker.clone().prop_map(|worker| WorkerDisconnected { worker }),
        6 => worker.prop_map(|worker| WorkerAsksSchedulerForBuildJob { worker }),
        4 => assignment.clone().prop_map(|assignment| WorkerReportsBuildStarted { assignment }),
        5 => assignment
            .clone()
            .prop_map(|assignment| UploadOutputAndReportBuildCompleted { assignment }),
        2 => (assignment, any::<bool>())
            .prop_map(|(assignment, permanent)| WorkerReportedBuildFailed { assignment, permanent }),
        1 => derivation
            .clone()
            .prop_map(|derivation| AdminAbortsBuildOfOneDerivation { derivation }),
        1 => Just(AdminAbortsWholeEvaluation),
        1 => derivation.prop_map(|derivation| AdminRetriesDerivationBuild { derivation }),
        2 => Just(UpstreamCacheProbeNotFoundForEveryWaitingBuild),
        2 => Just(RequeueTransientlyFailedBuilds),
    ]
}

fn build_test_database_shape_from_dependency_lists(
    evaluation: EvaluationId,
    dependencies: &[Vec<usize>],
) -> Shape {
    let has_dependent: Vec<bool> = (0..dependencies.len())
        .map(|at| dependencies.iter().any(|list| list.contains(&at)))
        .collect();
    let seed = shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building);
    dependencies
        .iter()
        .enumerate()
        .fold(seed, |seed, (at, list)| {
            let (path, out) = SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[at];
            let mut seed = seed.derivation(path).walked().probed().drv_cached();
            if !has_dependent[at] {
                seed = seed.entry_point(&attr_of(path));
            }
            seed = seed.job().output(out);
            for &dependency in list {
                seed = seed.depends_on(SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[dependency].0);
            }
            if list.is_empty() {
                seed = seed.build_status(BuildStatus::Queued);
            }
            seed
        })
}

async fn run_generated_input_catch_panic_and_delete_test_database(
    input: GeneratedDerivationDependencyAndActionTestInput,
) -> Result<(), TestCaseError> {
    let evaluation = EvaluationId::now_v7();
    let seed = build_test_database_shape_from_dependency_lists(
        evaluation,
        &input.derivation_dependency_indices,
    );
    let case = SchedulerCase::start_with(seed, |config| config.build.retry_backoff_secs = 0).await;
    let outcome = AssertUnwindSafe(
        apply_all_actions_check_invariants_after_each_then_finish_remaining(
            &case, evaluation, &input,
        ),
    )
    .catch_unwind()
    .await;
    match outcome {
        Ok(()) => {
            case.finish().await;
            Ok(())
        }
        Err(panic) => {
            case.stop_actors_and_delete_test_database_without_final_checks()
                .await;
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_default();
            Err(TestCaseError::fail(message))
        }
    }
}

async fn apply_all_actions_check_invariants_after_each_then_finish_remaining(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    input: &GeneratedDerivationDependencyAndActionTestInput,
) {
    let count = input.derivation_dependency_indices.len();
    let mut keys = Vec::with_capacity(count);
    for (path, _) in &SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[..count] {
        keys.push(case.key(path).await);
    }
    let mut book = ConnectedWorkersAndJobAssignments::default();
    for (index, action) in input.actions_to_run.iter().enumerate() {
        apply_action(case, evaluation, &keys, &mut book, *action).await;
        case.check(&format!("Action {index}: {action:?}")).await;
    }
    run_liveness_check(case, evaluation, &keys, book).await;
}

async fn apply_action(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    keys: &[String],
    book: &mut ConnectedWorkersAndJobAssignments,
    action: GeneratedSchedulerAction,
) {
    use GeneratedSchedulerAction::*;
    let held = book.assignments.len();
    match action {
        WorkerAtSlotConnectsWithOneOrTwoBuildSlots { worker, slots } => {
            if !book.connected[worker] {
                case.connect(ACTION_WORKER_NAMES[worker], w_all().slots(slots), &[])
                    .await;
                book.connected[worker] = true;
            }
        }
        WorkerDisconnected { worker } => {
            if book.connected[worker] {
                let name = ACTION_WORKER_NAMES[worker];
                case.disconnect(name).await;
                book.connected[worker] = false;
                book.assignments.retain(|held| held.worker != name);
            }
        }
        WorkerAsksSchedulerForBuildJob { worker } => {
            if book.connected[worker] {
                let name = ACTION_WORKER_NAMES[worker];
                case.admit_startable_moves().await;
                case.offers_full(name).await;
                case.score_all(name).await;
                if let Some(handed) = case.request_job(name, JobKind::Build).await {
                    book.assignments.push(assignment_of(
                        keys,
                        name,
                        handed.job_id(),
                        handed.assignment_id(),
                    ));
                }
            }
        }
        WorkerReportsBuildStarted { assignment } if held > 0 => {
            let held = &mut book.assignments[assignment % held];
            case.report_building(held.worker, &held.key).await;
            held.worker_reported_build_started = true;
        }
        UploadOutputAndReportBuildCompleted { assignment } if held > 0 => {
            let held = book.assignments.remove(assignment % held);
            report_building_upload_output_and_complete_assignment(case, held).await;
        }
        WorkerReportedBuildFailed {
            assignment,
            permanent,
        } if held > 0 => {
            let held = book.assignments.remove(assignment % held);
            if !held.worker_reported_build_started {
                case.report_building(held.worker, &held.key).await;
            }
            let kind = if permanent {
                BuildFailureKind::Permanent
            } else {
                BuildFailureKind::Transient
            };
            case.fail(held.worker, &held.key, held.assignment, failure(kind))
                .await;
        }
        AdminAbortsBuildOfOneDerivation { derivation } => {
            let (path, _) = SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[derivation % keys.len()];
            let _ = case.abort_build(evaluation, path).await;
        }
        AdminAbortsWholeEvaluation => case.abort_evaluation(evaluation).await,
        AdminRetriesDerivationBuild { derivation } => {
            let (path, _) = SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[derivation % keys.len()];
            let _ = case.retry_build(evaluation, path).await;
        }
        UpstreamCacheProbeNotFoundForEveryWaitingBuild => {
            answer_upstream_probe_for_each_created_build_waiting_for_probe(case, keys.len()).await;
        }
        RequeueTransientlyFailedBuilds => {
            case.requeue_transient_retries().await;
        }
        WorkerReportsBuildStarted { .. }
        | UploadOutputAndReportBuildCompleted { .. }
        | WorkerReportedBuildFailed { .. } => {}
    }
}

fn assignment_of(
    keys: &[String],
    worker: &'static str,
    key: &str,
    assignment: DispatchedJobId,
) -> WorkerJobAssignment {
    let derivation = keys
        .iter()
        .position(|known| known == key)
        .unwrap_or_else(|| {
            panic!("The scheduler passed {key}, but that key doesn't name a build of the generated graph.")
        });
    WorkerJobAssignment {
        worker,
        derivation,
        key: key.to_owned(),
        assignment,
        worker_reported_build_started: false,
    }
}

async fn report_building_upload_output_and_complete_assignment(
    case: &SchedulerCase,
    held: WorkerJobAssignment,
) {
    if !held.worker_reported_build_started {
        case.report_building(held.worker, &held.key).await;
    }
    let (path, out) = SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[held.derivation];
    case.commit_nar(output_nar(path, out)).await;
    case.complete(held.worker, &held.key, held.assignment, Vec::new(), 1)
        .await;
}

async fn answer_upstream_probe_for_each_created_build_waiting_for_probe(
    case: &SchedulerCase,
    count: usize,
) {
    let mut waiting = Vec::new();
    for (path, _) in &SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[..count] {
        let build = case.sb(path).await;
        if build.status == BuildStatus::Created && !build.probed {
            waiting.push(*path);
        }
    }
    if !waiting.is_empty() {
        case.upstream_probed(&waiting).await;
    }
}

async fn run_liveness_check(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    keys: &[String],
    book: ConnectedWorkersAndJobAssignments,
) {
    for held in book.assignments {
        report_building_upload_output_and_complete_assignment(case, held).await;
    }
    case.check("Assignments that workers still hold").await;

    let worker = WORKER_NAME_FOR_FINISHING_REMAINING_WORK;
    case.connect(worker, w_all(), &[]).await;
    for round in 0..3 * keys.len() + 6 {
        if !case.evaluation(evaluation).await.status.is_active() {
            break;
        }
        case.requeue_transient_retries().await;
        answer_upstream_probe_for_each_created_build_waiting_for_probe(case, keys.len()).await;
        case.admit_startable_moves().await;
        case.offers_full(worker).await;
        case.score_all(worker).await;
        if let Some(handed) = case.request_job(worker, JobKind::Build).await {
            let held = assignment_of(keys, worker, handed.job_id(), handed.assignment_id());
            report_building_upload_output_and_complete_assignment(case, held).await;
        }
        case.check(&format!("Liveness round {round}")).await;
    }

    let status = case.evaluation(evaluation).await.status;
    assert!(
        !status.is_active(),
        "The evaluation can still stay at {status:?} even after a well-behaved worker finished every build that it received."
    );
}

#[test]
fn test_random_action_sequences_on_random_derivation_graphs_preserve_scheduler_invariants_and_finish_evaluation()
 {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("The tokio runtime can start.");
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 24,
        max_shrink_iters: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    });
    let strategy = (
        derivation_dependency_lists_for_one_to_six_derivations(),
        proptest::collection::vec(action_strategy(), 0..=40),
    )
        .prop_map(|(derivation_dependency_indices, actions_to_run)| {
            GeneratedDerivationDependencyAndActionTestInput {
                derivation_dependency_indices,
                actions_to_run,
            }
        });
    if let Err(failure) = runner.run(&strategy, |input| {
        runtime.block_on(run_generated_input_catch_panic_and_delete_test_database(
            input,
        ))
    }) {
        panic!("{failure}");
    }
}
