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
    runtime_output_reference_derivation_dependency_indices: Vec<Vec<usize>>,
    generated_starting_states_by_derivation_index: Vec<InitialDerivationState>,
    actions_to_run: Vec<GeneratedSchedulerAction>,
}

#[derive(Clone, Copy, Debug)]
enum InitialDerivationState {
    DerivationNotBuiltAndAlreadyProbed,
    OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated,
    SharedBuildAlreadyCompletedAndOutputStoredInCache,
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

fn generated_graph_and_actions()
-> impl Strategy<Value = GeneratedDerivationDependencyAndActionTestInput> {
    use InitialDerivationState::*;
    (1..=SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES.len()).prop_flat_map(|count| {
        (
            acyclic_dependency_index_lists_strategy_for_derivation_count(count),
            acyclic_dependency_index_lists_strategy_for_derivation_count(count),
            prop_oneof![
                3 => Just(DerivationNotBuiltAndAlreadyProbed),
                1 => Just(OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated),
            ],
            proptest::collection::vec(
                prop_oneof![
                    3 => Just(DerivationNotBuiltAndAlreadyProbed),
                    1 => Just(OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated),
                    1 => Just(SharedBuildAlreadyCompletedAndOutputStoredInCache),
                ],
                count - 1,
            ),
            proptest::collection::vec(action_strategy(), 0..=40),
        )
            .prop_map(|(build, runtime, first, rest, actions_to_run)| {
                GeneratedDerivationDependencyAndActionTestInput {
                    derivation_dependency_indices: build,
                    runtime_output_reference_derivation_dependency_indices: runtime,
                    generated_starting_states_by_derivation_index: std::iter::once(first)
                        .chain(rest)
                        .collect(),
                    actions_to_run,
                }
            })
    })
}

fn acyclic_dependency_index_lists_strategy_for_derivation_count(
    count: usize,
) -> impl Strategy<Value = Vec<Vec<usize>>> {
    proptest::collection::vec(proptest::collection::vec(any::<bool>(), count), count).prop_map(
        move |edges| {
            edges
                .iter()
                .enumerate()
                .map(|(from, row)| (from + 1..count).filter(|&to| row[to]).collect())
                .collect()
        },
    )
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
    input: &GeneratedDerivationDependencyAndActionTestInput,
) -> Shape {
    use InitialDerivationState::*;
    let dependencies = &input.derivation_dependency_indices;
    let states = &input.generated_starting_states_by_derivation_index;
    let not_built = |at: usize| matches!(states[at], DerivationNotBuiltAndAlreadyProbed);
    let runtime_dependency_indices_with_cached_output_to_uncached_output_edges_removed: Vec<
        Vec<usize>,
    > = input
        .runtime_output_reference_derivation_dependency_indices
        .iter()
        .enumerate()
        .map(|(at, list)| {
            list.iter()
                .copied()
                .filter(|&dependency| not_built(at) || !not_built(dependency))
                .collect()
        })
        .collect();
    let runtime = &runtime_dependency_indices_with_cached_output_to_uncached_output_edges_removed;
    let has_dependent: Vec<bool> = (0..dependencies.len())
        .map(|at| {
            dependencies
                .iter()
                .enumerate()
                .any(|(from, list)| list.contains(&at) && not_built(from))
        })
        .collect();
    let seed = shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building);
    dependencies
        .iter()
        .enumerate()
        .fold(seed, |seed, (at, list)| {
            let (path, out) = SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES[at];
            let state = states[at];
            let mut seed = seed.derivation(path).walked().drv_cached();
            if !matches!(
                state,
                OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated
            ) {
                seed = seed.probed();
            }
            if !has_dependent[at] {
                seed = seed.entry_point(&attr_of(path));
            }
            seed = seed.job().output(out);
            for (dependency, (dependency_path, _)) in SIX_DERIVATION_STORE_PATHS_AND_OUTPUT_HASHES
                [..dependencies.len()]
                .iter()
                .enumerate()
            {
                seed = match (
                    list.contains(&dependency),
                    runtime[at].contains(&dependency),
                ) {
                    (true, true) => seed.both_depends_on(dependency_path),
                    (true, false) => seed.depends_on(dependency_path),
                    (false, true) => seed.runtime_depends_on(dependency_path),
                    (false, false) => seed,
                };
            }
            match state {
                DerivationNotBuiltAndAlreadyProbed
                    if list.iter().all(|&dependency| {
                        matches!(
                            states[dependency],
                            SharedBuildAlreadyCompletedAndOutputStoredInCache
                        )
                    }) =>
                {
                    seed.build_status(BuildStatus::Queued)
                }
                DerivationNotBuiltAndAlreadyProbed => seed,
                OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated => {
                    seed.cached_output(path)
                }
                SharedBuildAlreadyCompletedAndOutputStoredInCache => seed
                    .cached_output(path)
                    .build_status(BuildStatus::Completed),
            }
        })
}

async fn run_generated_input_catch_panic_and_delete_test_database(
    input: GeneratedDerivationDependencyAndActionTestInput,
) -> Result<(), TestCaseError> {
    let evaluation = EvaluationId::now_v7();
    let seed = build_test_database_shape_from_dependency_lists(evaluation, &input);
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
                case.connect(ACTION_WORKER_NAMES[worker], w_all().slots(slots))
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
    case.connect(worker, w_all()).await;
    for round in 0..3 * keys.len() + 6 {
        if !case.evaluation(evaluation).await.status.is_active() {
            break;
        }
        case.requeue_transient_retries().await;
        answer_upstream_probe_for_each_created_build_waiting_for_probe(case, keys.len()).await;
        case.admit_startable_moves().await;
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

fn build_generated_derivation_dependency_and_action_test_input_with_not_built_already_probed_starts_and_no_runtime_output_references(
    derivation_dependency_indices: Vec<Vec<usize>>,
    actions_to_run: Vec<GeneratedSchedulerAction>,
) -> GeneratedDerivationDependencyAndActionTestInput {
    let derivation_count = derivation_dependency_indices.len();
    GeneratedDerivationDependencyAndActionTestInput {
        derivation_dependency_indices,
        runtime_output_reference_derivation_dependency_indices: vec![vec![]; derivation_count],
        generated_starting_states_by_derivation_index: vec![
            InitialDerivationState::DerivationNotBuiltAndAlreadyProbed;
            derivation_count
        ],
        actions_to_run,
    }
}

#[test]
fn replay_shrunk_failing_action_list_check_consistency_and_evaluation_finish() {
    use GeneratedSchedulerAction::*;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("The tokio runtime can start.");
    let input = build_generated_derivation_dependency_and_action_test_input_with_not_built_already_probed_starts_and_no_runtime_output_references(
        vec![vec![1], vec![]],
        vec![
            UploadOutputAndReportBuildCompleted { assignment: 2 },
            WorkerAtSlotConnectsWithOneOrTwoBuildSlots {
                worker: 2,
                slots: 1,
            },
            WorkerAsksSchedulerForBuildJob { worker: 2 },
            UpstreamCacheProbeNotFoundForEveryWaitingBuild,
            WorkerAsksSchedulerForBuildJob { worker: 0 },
            AdminRetriesDerivationBuild { derivation: 2 },
            UpstreamCacheProbeNotFoundForEveryWaitingBuild,
            AdminAbortsBuildOfOneDerivation { derivation: 2 },
            WorkerReportedBuildFailed {
                assignment: 0,
                permanent: false,
            },
            WorkerAtSlotConnectsWithOneOrTwoBuildSlots {
                worker: 1,
                slots: 1,
            },
            UpstreamCacheProbeNotFoundForEveryWaitingBuild,
            WorkerAsksSchedulerForBuildJob { worker: 1 },
            WorkerAtSlotConnectsWithOneOrTwoBuildSlots {
                worker: 1,
                slots: 2,
            },
            RequeueTransientlyFailedBuilds,
            WorkerAsksSchedulerForBuildJob { worker: 1 },
            RequeueTransientlyFailedBuilds,
            RequeueTransientlyFailedBuilds,
        ],
    );
    if let Err(failure) = runtime.block_on(
        run_generated_input_catch_panic_and_delete_test_database(input),
    ) {
        panic!("{failure}");
    }
}

async fn assert_evaluation_leaves_building_after_aborted_dependent_and_failed_build(
    substitute_miss_failure_count_for_b_before_aborting_a: usize,
    failure_kind_reported_for_b_after_admin_aborted_a: BuildFailureKind,
    is_b_output_in_upstream_binary_cache: bool,
) {
    let evaluation = EvaluationId::now_v7();
    let mut seed = build_test_database_shape_from_dependency_lists(
        evaluation,
        &build_generated_derivation_dependency_and_action_test_input_with_not_built_already_probed_starts_and_no_runtime_output_references(
            vec![vec![1], vec![]],
            vec![],
        ),
    );
    if is_b_output_in_upstream_binary_cache {
        seed = seed.cache_available();
    }
    let case = SchedulerCase::start_with(seed, |config| config.build.retry_backoff_secs = 0).await;
    let keys = vec![case.key(A).await, case.key(B).await];
    let worker = ACTION_WORKER_NAMES[0];
    case.connect(worker, w_all()).await;
    for round in 0..=substitute_miss_failure_count_for_b_before_aborting_a {
        case.admit_startable_moves().await;
        let handed = case
            .request_job(worker, JobKind::Build)
            .await
            .expect("The worker can take the job for the remaining build.");
        let held = assignment_of(&keys, worker, handed.job_id(), handed.assignment_id());
        case.report_building(held.worker, &held.key).await;
        let kind = if round < substitute_miss_failure_count_for_b_before_aborting_a {
            BuildFailureKind::SubstituteUnavailable
        } else {
            let _ = case.abort_build(evaluation, A).await;
            failure_kind_reported_for_b_after_admin_aborted_a
        };
        case.fail(held.worker, &held.key, held.assignment, failure(kind))
            .await;
    }
    case.requeue_transient_retries().await;
    case.check("The worker reported failures for the remaining builds. The retry pass ran.")
        .await;
    run_liveness_check(
        &case,
        evaluation,
        &keys,
        ConnectedWorkersAndJobAssignments::default(),
    )
    .await;
    case.finish().await;
}

#[tokio::test]
async fn substitute_miss_on_only_remaining_build_after_aborting_dependent_requeues_build_and_evaluation_leaves_building()
 {
    assert_evaluation_leaves_building_after_aborted_dependent_and_failed_build(
        0,
        BuildFailureKind::SubstituteUnavailable,
        true,
    )
    .await;
}

#[tokio::test]
async fn canceled_build_after_aborting_only_dependent_returns_to_queued_and_evaluation_leaves_building()
 {
    assert_evaluation_leaves_building_after_aborted_dependent_and_failed_build(
        0,
        BuildFailureKind::Canceled,
        false,
    )
    .await;
}

#[tokio::test]
async fn test_evaluation_leaves_building_when_substitute_miss_exceeds_limit_for_only_remaining_build_after_admin_aborted_its_only_dependent()
 {
    assert_evaluation_leaves_building_after_aborted_dependent_and_failed_build(
        1,
        BuildFailureKind::SubstituteUnavailable,
        true,
    )
    .await;
}

#[tokio::test]
async fn aborting_only_dependent_then_worker_disconnect_returns_build_to_queued_and_evaluation_leaves_building()
 {
    let evaluation = EvaluationId::now_v7();
    let seed = build_test_database_shape_from_dependency_lists(
        evaluation,
        &build_generated_derivation_dependency_and_action_test_input_with_not_built_already_probed_starts_and_no_runtime_output_references(
            vec![vec![1], vec![]],
            vec![],
        ),
    );
    let case = SchedulerCase::start(seed).await;
    let keys = vec![case.key(A).await, case.key(B).await];
    let worker = ACTION_WORKER_NAMES[0];
    case.connect(worker, w_all()).await;
    case.admit_startable_moves().await;
    let handed = case
        .request_job(worker, JobKind::Build)
        .await
        .expect("The worker can take the job for the remaining build.");
    let held = assignment_of(&keys, worker, handed.job_id(), handed.assignment_id());
    case.report_building(held.worker, &held.key).await;
    let _ = case.abort_build(evaluation, A).await;
    case.disconnect(worker).await;
    case.requeue_transient_retries().await;
    case.check("The worker disconnected from the remaining build. The retry pass ran.")
        .await;
    run_liveness_check(
        &case,
        evaluation,
        &keys,
        ConnectedWorkersAndJobAssignments::default(),
    )
    .await;
    case.finish().await;
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
    if let Err(failure) = runner.run(&generated_graph_and_actions(), |input| {
        runtime.block_on(run_generated_input_catch_panic_and_delete_test_database(
            input,
        ))
    }) {
        panic!("{failure}");
    }
}

#[test]
fn replay_saved_failing_six_derivation_action_list_ending_with_admin_abort_after_retry_check_consistency_after_each_action_and_evaluation_finish()
 {
    use GeneratedSchedulerAction::*;
    use InitialDerivationState::*;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("The tokio runtime can start.");
    let input = GeneratedDerivationDependencyAndActionTestInput {
        derivation_dependency_indices: vec![vec![]; 6],
        runtime_output_reference_derivation_dependency_indices: vec![
            vec![4],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        ],
        generated_starting_states_by_derivation_index: vec![
            OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated,
            DerivationNotBuiltAndAlreadyProbed,
            OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated,
            SharedBuildAlreadyCompletedAndOutputStoredInCache,
            OutputInOwnCacheUpstreamCacheProbeUnansweredBuildCreated,
            DerivationNotBuiltAndAlreadyProbed,
        ],
        actions_to_run: vec![
            WorkerReportsBuildStarted { assignment: 7 },
            UploadOutputAndReportBuildCompleted { assignment: 4 },
            WorkerAsksSchedulerForBuildJob { worker: 1 },
            UploadOutputAndReportBuildCompleted { assignment: 4 },
            UploadOutputAndReportBuildCompleted { assignment: 2 },
            AdminRetriesDerivationBuild { derivation: 4 },
            RequeueTransientlyFailedBuilds,
            UpstreamCacheProbeNotFoundForEveryWaitingBuild,
            WorkerReportsBuildStarted { assignment: 0 },
            AdminAbortsBuildOfOneDerivation { derivation: 4 },
            WorkerAtSlotConnectsWithOneOrTwoBuildSlots {
                worker: 1,
                slots: 1,
            },
            WorkerAtSlotConnectsWithOneOrTwoBuildSlots {
                worker: 2,
                slots: 1,
            },
            WorkerAsksSchedulerForBuildJob { worker: 2 },
            WorkerAsksSchedulerForBuildJob { worker: 1 },
            WorkerAsksSchedulerForBuildJob { worker: 0 },
        ],
    };
    if let Err(failure) = runtime.block_on(
        run_generated_input_catch_panic_and_delete_test_database(input),
    ) {
        panic!("{failure}");
    }
}
