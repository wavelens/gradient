/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gradient_pool::WorkerCaps;
use gradient_pool::score::{InstanceContext, policy_for};
use gradient_scheduler::jobs::{JobTracker, PendingBuildJob, PendingJob, build_job_key};
use gradient_scheduler::pending_job_input_store_paths::PendingJobInputStorePath;
use gradient_types::ids::{DerivationBuildId, DerivationId, EvaluationId, ProjectId};
use gradient_wire::types::{
    BuildJob, BuildRequirement, BuildSpec, BuildSpecKind, GradientCapabilities, JobKind,
};
use gradient_wire::worker_store_path_bloom_filter::{
    FILTER_BITS_PER_STORE_PATH, WorkerStorePathBloomFilter,
};

const STORE_PATH_PROBE_WORD_COUNT_IN_INPUT_POOL: usize = 200_000;
const PENDING_BUILD_COUNT: usize = 50_000;
const DIRECT_INPUTS_PER_PENDING_BUILD: usize = 20;
const NUMBER_OF_PROJECTS: usize = 20;
const SUPPORTED_BUILD_ARCHITECTURES: [&str; 2] = ["x86_64-linux", "aarch64-linux"];
const WORKER_COUNT: usize = 200;
const STORE_PATHS_PER_WORKER_BLOOM_FILTER: usize = 100_000;
const WORKER_ONE_JOB_REQUEST_ROUND_COUNT: usize = 5;
const WORKER_DISTINCT_BLOOM_FILTER_CLONE_COUNT: usize = 4;
#[expect(
    clippy::cast_possible_truncation,
    reason = "A filter of 100,000 paths is 250,000 bytes."
)]
const WORKER_BLOOM_FILTER_BYTE_SIZE_FOR_100_000_PATHS: usize =
    (STORE_PATHS_PER_WORKER_BLOOM_FILTER as u64 * FILTER_BITS_PER_STORE_PATH).div_ceil(8) as usize;

struct AllocatedByteCountingAllocator {
    currently_allocated_bytes: AtomicUsize,
    peak_allocated_bytes: AtomicUsize,
}

impl AllocatedByteCountingAllocator {
    fn add_allocated_bytes_and_raise_peak(&self, bytes: usize) {
        let now = self
            .currently_allocated_bytes
            .fetch_add(bytes, Ordering::Relaxed)
            + bytes;
        self.peak_allocated_bytes.fetch_max(now, Ordering::Relaxed);
    }

    fn reset_peak_to_current_allocated_byte_count_and_return_baseline(&self) -> usize {
        let baseline = self.currently_allocated_bytes.load(Ordering::Relaxed);
        self.peak_allocated_bytes.store(baseline, Ordering::Relaxed);
        baseline
    }
}

unsafe impl GlobalAlloc for AllocatedByteCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            self.add_allocated_bytes_and_raise_peak(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            self.add_allocated_bytes_and_raise_peak(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        self.currently_allocated_bytes
            .fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            self.currently_allocated_bytes
                .fetch_sub(layout.size(), Ordering::Relaxed);
            self.add_allocated_bytes_and_raise_peak(new_size);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATED_BYTE_COUNTING_ALLOCATOR: AllocatedByteCountingAllocator =
    AllocatedByteCountingAllocator {
        currently_allocated_bytes: AtomicUsize::new(0),
        peak_allocated_bytes: AtomicUsize::new(0),
    };

fn next_splitmix64_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn build_deterministic_store_path_probe_word_pair_pool() -> Vec<[u64; 2]> {
    let mut state = 1;
    (0..STORE_PATH_PROBE_WORD_COUNT_IN_INPUT_POOL)
        .map(|_| {
            [
                next_splitmix64_u64(&mut state),
                next_splitmix64_u64(&mut state),
            ]
        })
        .collect()
}

fn create_job_tracker_with_50000_pending_builds(pool: &[[u64; 2]]) -> JobTracker {
    let mut state = 2;
    let projects: Vec<ProjectId> = (0..NUMBER_OF_PROJECTS)
        .map(|_| ProjectId::now_v7())
        .collect();
    let now = gradient_types::now();
    let mut tracker = JobTracker::new();
    for index in 0..PENDING_BUILD_COUNT {
        let mut random = || next_splitmix64_u64(&mut state);
        let inputs: Vec<PendingJobInputStorePath> = (0..DIRECT_INPUTS_PER_PENDING_BUILD)
            .map(|_| PendingJobInputStorePath {
                store_path_bloom_filter_probe_words: Some(
                    pool[usize::try_from(random()).unwrap_or_default() % pool.len()],
                ),
                nar_size: random() % (64 << 20),
            })
            .collect();
        let ready_at = now - chrono::Duration::seconds((random() % 86_400) as i64);
        let derivation_build = DerivationBuildId::now_v7();
        let architecture =
            SUPPORTED_BUILD_ARCHITECTURES[index % SUPPORTED_BUILD_ARCHITECTURES.len()];
        let job = PendingJob::Build(PendingBuildJob {
            derivation_build,
            derivation: DerivationId::now_v7(),
            evaluation_id: EvaluationId::now_v7(),
            project_id: projects[index % NUMBER_OF_PROJECTS],
            job: BuildJob {
                build: BuildSpec {
                    build_id: derivation_build.to_string(),
                    drv_path: format!("/nix/store/{index:032}-package.drv"),
                    kind: BuildSpecKind::Build,
                    is_fixed_output: false,
                    outputs: Vec::new(),
                    timeout_secs: None,
                    max_silent_secs: None,
                    build_cores: None,
                    input_realisations: Vec::new(),
                },
                requirement: BuildRequirement {
                    architecture: architecture.into(),
                    required_features: Vec::new(),
                },
            },
            shared_direct_input_store_paths_with_probe_words_and_nar_sizes: inputs.into(),
            output_path_bloom_filter_probe_words_after_store_path_mapping: Default::default(),
            dependency_count: 0,
            closure_size: None,
            prefer_local_build: false,
            is_fixed_output: false,
            estimate: Default::default(),
            history_key: None,
            queued_at: ready_at,
            ready_at,
            rescore_count: 0,
            prioritized: false,
            build_request: false,
            ifd: false,
            pname: Some(format!("package-{index}")),
            substitute: false,
            verifying_keys: Vec::new(),
        });
        tracker.add_pending(build_job_key(derivation_build), job);
    }
    tracker
}

fn build_worker_store_path_bloom_filters(pool: &[[u64; 2]]) -> Vec<WorkerStorePathBloomFilter> {
    let stride = pool.len() / WORKER_DISTINCT_BLOOM_FILTER_CLONE_COUNT;
    (0..WORKER_DISTINCT_BLOOM_FILTER_CLONE_COUNT)
        .map(|template| {
            let mut filter = WorkerStorePathBloomFilter::new_for_store_path_count(
                STORE_PATHS_PER_WORKER_BLOOM_FILTER,
            );
            (0..STORE_PATHS_PER_WORKER_BLOOM_FILTER)
                .map(|offset| pool[(template * stride + offset) % pool.len()])
                .for_each(|words| filter.insert_probe_words_into_filter(words));
            filter
        })
        .collect()
}

fn build_worker_caps_for_arch(architecture: &str) -> WorkerCaps {
    WorkerCaps {
        architectures: vec![architecture.into()],
        capabilities: GradientCapabilities {
            build: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

struct ScaleTestRunRecord {
    total_requests_wall_clock_time: Duration,
    peak_allocated_bytes_above_pre_worker_filter_baseline: usize,
    assigned_job_ids: Vec<String>,
}

fn run_request_rounds_for_workers_and_return_elapsed_time_peak_allocation_growth_and_job_ids(
    worker_count: usize,
    requests_per_worker: usize,
) -> ScaleTestRunRecord {
    let pool = build_deterministic_store_path_probe_word_pair_pool();
    let mut tracker = create_job_tracker_with_50000_pending_builds(&pool);
    let templates = build_worker_store_path_bloom_filters(&pool);
    let policy = policy_for(gradient_types::ScoringPolicy::ResourceAware);
    let instance = InstanceContext::default();
    let worker_ids: Vec<String> = (0..worker_count)
        .map(|index| format!("worker-{index}"))
        .collect();
    let caps: Vec<WorkerCaps> = (0..worker_count)
        .map(|index| {
            build_worker_caps_for_arch(
                SUPPORTED_BUILD_ARCHITECTURES[index % SUPPORTED_BUILD_ARCHITECTURES.len()],
            )
        })
        .collect();
    let mut assigned_job_ids = Vec::with_capacity(worker_count * requests_per_worker);

    let baseline = ALLOCATED_BYTE_COUNTING_ALLOCATOR
        .reset_peak_to_current_allocated_byte_count_and_return_baseline();
    let filters: Vec<WorkerStorePathBloomFilter> = (0..worker_count)
        .map(|index| templates[index % templates.len()].clone())
        .collect();
    let started = Instant::now();
    for _ in 0..requests_per_worker {
        for worker in 0..worker_count {
            let assignment = tracker.take_best_of_kind(
                &worker_ids[worker],
                None,
                Some(&caps[worker]),
                Some(&filters[worker]),
                &JobKind::Build,
                &*policy,
                &instance,
                &[],
            );
            assigned_job_ids.extend(assignment.map(|a| a.job_id().to_owned()));
        }
    }
    let total_requests_wall_clock_time = started.elapsed();
    let peak = ALLOCATED_BYTE_COUNTING_ALLOCATOR
        .peak_allocated_bytes
        .load(Ordering::Relaxed);

    ScaleTestRunRecord {
        total_requests_wall_clock_time,
        peak_allocated_bytes_above_pre_worker_filter_baseline: peak.saturating_sub(baseline),
        assigned_job_ids,
    }
}

#[test]
fn fifty_thousand_pending_builds_with_two_hundred_bloom_filter_workers_stay_within_time_and_memory_budgets()
 {
    let all_workers =
        run_request_rounds_for_workers_and_return_elapsed_time_peak_allocation_growth_and_job_ids(
            WORKER_COUNT,
            WORKER_ONE_JOB_REQUEST_ROUND_COUNT,
        );
    // A single worker can send the same number of requests as all 200 workers together. Both runs then keep the same decision log and the same assigned jobs. The extra Bloom filters alone create the memory difference.
    let one_worker =
        run_request_rounds_for_workers_and_return_elapsed_time_peak_allocation_growth_and_job_ids(
            1,
            WORKER_COUNT * WORKER_ONE_JOB_REQUEST_ROUND_COUNT,
        );

    let elapsed = all_workers.total_requests_wall_clock_time;
    assert!(
        elapsed < Duration::from_secs(60),
        "1000 requests took {elapsed:?}"
    );

    let growth = all_workers.peak_allocated_bytes_above_pre_worker_filter_baseline;
    let budget = WORKER_COUNT * WORKER_BLOOM_FILTER_BYTE_SIZE_FOR_100_000_PATHS + (64 << 20);
    assert!(
        growth < budget,
        "Peak allocation rose {growth} bytes above baseline. The budget was {budget} bytes."
    );

    let difference =
        growth.saturating_sub(one_worker.peak_allocated_bytes_above_pre_worker_filter_baseline);
    let total_filter_bytes = WORKER_COUNT * WORKER_BLOOM_FILTER_BYTE_SIZE_FOR_100_000_PATHS;
    let expected = (WORKER_COUNT - 1) * WORKER_BLOOM_FILTER_BYTE_SIZE_FOR_100_000_PATHS;
    assert!(
        difference.abs_diff(expected) <= total_filter_bytes / 10,
        "Memory use at 200 workers increased by {difference} bytes more than at 1 worker. The extra filters use {expected} bytes."
    );

    for run in [&all_workers, &one_worker] {
        assert_eq!(
            run.assigned_job_ids.len(),
            WORKER_COUNT * WORKER_ONE_JOB_REQUEST_ROUND_COUNT,
            "Only some requests received a job"
        );
    }

    let distinct: HashSet<&String> = all_workers.assigned_job_ids.iter().collect();
    assert_eq!(
        distinct.len(),
        all_workers.assigned_job_ids.len(),
        "Job assigned twice"
    );
}
