/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use gradient_wire::types::{ExecInventory, ExecRequirement};

use crate::score::ScoreBreakdown;
use crate::score::weights::{
    EXEC_DEVICE_FIT_POINTS, EXEC_PROGRAM_TRANSFER_CAP_SECS, EXEC_PROGRAM_TRANSFER_MBPS,
    EXEC_WAIT_POINTS_PER_SEC, EXEC_WAIT_TIME_CAP, EXEC_WARM_GUEST_HOLD_ROUNDS,
    EXEC_WARM_GUEST_POINTS,
};

pub const WARM_GUEST: &str = "ExecWarmGuest";
pub const PROGRAM_CACHED: &str = "ExecProgramCached";
pub const DEVICE_FIT: &str = "ExecDeviceFit";
pub const WAIT_TIME: &str = "ExecWaitTime";
pub const WARM_GUEST_ELSEWHERE: &str = "ExecWarmGuestElsewhere";

pub const CATALOG: [(&str, &str); 5] = [
    (
        WARM_GUEST,
        "Prefers the worker with a warm guest of the requested size, saving the guest boot.",
    ),
    (
        PROGRAM_CACHED,
        "Prefers the worker that already holds the program closure, falling with the bytes left to download.",
    ),
    (
        DEVICE_FIT,
        "Grows with the share of the idle devices of the worker the exec job uses, keeping devices free for the jobs asking for them.",
    ),
    (
        WAIT_TIME,
        "Grows with how long an exec job has been waiting, so no job starves behind better-placed ones.",
    ),
    (
        WARM_GUEST_ELSEWHERE,
        "Briefly holds an exec job back from a worker without a usable warm guest while another worker has one.",
    ),
];

#[derive(Clone, Copy, Debug)]
pub struct ExecCandidate<'a> {
    pub requirement: &'a ExecRequirement,
    pub missing_nar_size: Option<u64>,
    pub waited_secs: i64,
    pub rescore_count: u32,
    pub warm_guest_elsewhere: bool,
}

pub fn score(job: &ExecCandidate<'_>, worker: &ExecInventory) -> ScoreBreakdown {
    let rules = BTreeMap::from([
        (WARM_GUEST.to_owned(), warm_guest(job.requirement, worker)),
        (
            PROGRAM_CACHED.to_owned(),
            program_cached(job.missing_nar_size),
        ),
        (DEVICE_FIT.to_owned(), device_fit(job.requirement, worker)),
        (WAIT_TIME.to_owned(), wait_time(job.waited_secs)),
    ]);
    let vetoes = waits_for_warm_guest_elsewhere(job, worker)
        .then(|| WARM_GUEST_ELSEWHERE.to_owned())
        .into_iter()
        .collect();
    ScoreBreakdown {
        total: rules.values().sum(),
        rules,
        vetoes,
        estimate: None,
    }
}

pub fn has_usable_warm_guest(requirement: &ExecRequirement, worker: &ExecInventory) -> bool {
    !requirement.custom_image
        && worker.warm_guests > 0
        && (requirement.guest.is_unset() || worker.has_warm_guest_for(requirement.guest))
}

fn warm_guest(requirement: &ExecRequirement, worker: &ExecInventory) -> f64 {
    match has_usable_warm_guest(requirement, worker) {
        true => EXEC_WARM_GUEST_POINTS,
        false => 0.0,
    }
}

fn program_cached(missing_nar_size: Option<u64>) -> f64 {
    let Some(bytes) = missing_nar_size else {
        return 0.0;
    };
    let transfer_secs = bytes as f64 / (EXEC_PROGRAM_TRANSFER_MBPS * 1_000_000.0);
    (EXEC_PROGRAM_TRANSFER_CAP_SECS - transfer_secs).max(0.0)
}

fn device_fit(requirement: &ExecRequirement, worker: &ExecInventory) -> f64 {
    let idle: u32 = worker.devices.iter().map(|class| class.free).sum();
    if idle == 0 {
        return EXEC_DEVICE_FIT_POINTS;
    }

    let used: u32 = requirement
        .devices
        .iter()
        .map(|request| request.count)
        .sum();
    EXEC_DEVICE_FIT_POINTS * f64::from(used.min(idle)) / f64::from(idle)
}

fn wait_time(waited_secs: i64) -> f64 {
    (waited_secs.max(0) as f64 * EXEC_WAIT_POINTS_PER_SEC).min(EXEC_WAIT_TIME_CAP)
}

fn waits_for_warm_guest_elsewhere(job: &ExecCandidate<'_>, worker: &ExecInventory) -> bool {
    job.warm_guest_elsewhere
        && job.rescore_count < EXEC_WARM_GUEST_HOLD_ROUNDS
        && !has_usable_warm_guest(job.requirement, worker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::{DeviceClass, DeviceRequest, GuestSize};

    fn small() -> GuestSize {
        GuestSize::default_request()
    }

    fn large() -> GuestSize {
        GuestSize {
            cpus: 16,
            memory_mb: 65_536,
            ..GuestSize::default_request()
        }
    }

    fn worker_with_warm_guests(count: u32, size: GuestSize) -> ExecInventory {
        ExecInventory {
            warm_guests: count,
            warm_guest_size: size,
            ..ExecInventory::default()
        }
    }

    fn gpu_worker(idle: u32) -> ExecInventory {
        ExecInventory {
            devices: vec![DeviceClass {
                name: "gpu:h100".into(),
                total: 8,
                free: idle,
            }],
            ..ExecInventory::default()
        }
    }

    fn requirement(edit: impl FnOnce(&mut ExecRequirement)) -> ExecRequirement {
        let mut requirement = ExecRequirement {
            guest: small(),
            ..ExecRequirement::default()
        };
        edit(&mut requirement);
        requirement
    }

    fn gpus(count: u32) -> ExecRequirement {
        requirement(|r| {
            r.devices = vec![DeviceRequest {
                name: "gpu:h100".into(),
                count,
            }]
        })
    }

    fn candidate(requirement: &ExecRequirement) -> ExecCandidate<'_> {
        ExecCandidate {
            requirement,
            missing_nar_size: None,
            waited_secs: 0,
            rescore_count: 0,
            warm_guest_elsewhere: false,
        }
    }

    fn total(job: &ExecCandidate<'_>, worker: &ExecInventory) -> f64 {
        score(job, worker).total
    }

    #[test]
    fn a_warm_guest_counts_only_when_its_size_and_image_match_the_job() {
        let plain = requirement(|_| {});
        let custom_image = requirement(|r| r.custom_image = true);
        let warm_small = worker_with_warm_guests(1, small());

        assert!(has_usable_warm_guest(&plain, &warm_small));
        assert!(!has_usable_warm_guest(&custom_image, &warm_small));
        assert!(!has_usable_warm_guest(
            &plain,
            &worker_with_warm_guests(1, large())
        ));
        assert!(!has_usable_warm_guest(
            &plain,
            &worker_with_warm_guests(0, small())
        ));
        assert!(
            !has_usable_warm_guest(&plain, &worker_with_warm_guests(1, GuestSize::default())),
            "a worker without a reported warm size offers no usable guest"
        );
    }

    #[test]
    fn a_warm_guest_of_a_second_size_serves_a_job_of_that_size() {
        let worker = ExecInventory {
            warm_guest_sizes: gradient_wire::types::count_warm_guests([(small(), 1), (large(), 1)]),
            ..worker_with_warm_guests(2, small())
        };
        assert!(has_usable_warm_guest(
            &requirement(|r| r.guest = large()),
            &worker
        ));
    }

    #[test]
    fn a_disk_size_apart_still_takes_the_warm_guest() {
        let bigger_disk = requirement(|r| r.guest.disk_mb = 100_000);
        assert!(has_usable_warm_guest(
            &bigger_disk,
            &worker_with_warm_guests(1, small())
        ));
    }

    #[test]
    fn a_job_without_a_size_takes_any_warm_guest_like_the_worker_does() {
        let unsized_job = requirement(|r| r.guest = GuestSize::default());
        assert!(has_usable_warm_guest(
            &unsized_job,
            &worker_with_warm_guests(1, large())
        ));
    }

    #[test]
    fn a_worker_with_a_usable_warm_guest_outranks_an_equal_cold_one() {
        let plain = requirement(|_| {});
        let job = candidate(&plain);
        let warm = total(&job, &worker_with_warm_guests(1, small()));
        let cold = total(&job, &worker_with_warm_guests(0, small()));
        let wrong_size = total(&job, &worker_with_warm_guests(2, large()));

        assert!(warm > cold, "warm={warm} cold={cold}");
        assert_eq!(cold, wrong_size, "an unusable warm guest earns nothing");
    }

    #[test]
    fn a_cached_program_beats_one_still_to_download() {
        let plain = requirement(|_| {});
        let worker = ExecInventory::default();
        let at = |missing| {
            total(
                &ExecCandidate {
                    missing_nar_size: missing,
                    ..candidate(&plain)
                },
                &worker,
            )
        };

        assert!(at(Some(0)) > at(Some(5_000_000_000)));
        assert!(at(Some(5_000_000_000)) > at(Some(500_000_000_000)));
        assert_eq!(
            at(Some(500_000_000_000)),
            at(None),
            "the bonus bottoms out at zero"
        );
    }

    #[test]
    fn a_gpu_worker_prefers_the_job_that_uses_its_gpus() {
        let worker = gpu_worker(8);
        let cpu_only = requirement(|_| {});
        let all_gpus = gpus(8);
        let two_gpus = gpus(2);

        let cpu = total(&candidate(&cpu_only), &worker);
        let two = total(&candidate(&two_gpus), &worker);
        let eight = total(&candidate(&all_gpus), &worker);
        assert!(
            eight > two && two > cpu,
            "eight={eight} two={two} cpu={cpu}"
        );
    }

    #[test]
    fn a_worker_without_idle_devices_fits_a_job_like_a_worker_without_devices() {
        let cpu_only = requirement(|_| {});
        assert_eq!(
            device_fit(&cpu_only, &ExecInventory::default()),
            device_fit(&cpu_only, &gpu_worker(0))
        );
        assert_eq!(
            device_fit(&cpu_only, &gpu_worker(0)),
            device_fit(&gpus(8), &gpu_worker(8))
        );
    }

    #[test]
    fn a_long_wait_lifts_a_badly_placed_job_over_a_fresh_well_placed_one() {
        let worker = gpu_worker(8);
        let cpu_only = requirement(|_| {});
        let all_gpus = gpus(8);
        let starving = ExecCandidate {
            waited_secs: 3 * 3600,
            ..candidate(&cpu_only)
        };

        assert!(total(&starving, &worker) > total(&candidate(&all_gpus), &worker));
    }

    #[test]
    fn every_placement_clears_the_assign_floor() {
        let custom_image = requirement(|r| r.custom_image = true);
        let job = ExecCandidate {
            missing_nar_size: Some(u64::MAX),
            waited_secs: -5,
            ..candidate(&custom_image)
        };
        assert!(total(&job, &gpu_worker(8)) >= crate::score::weights::ASSIGN_FLOOR);
    }

    #[test]
    fn a_cold_worker_holds_the_job_for_a_warm_one_for_a_few_rounds_only() {
        let plain = requirement(|_| {});
        let cold = worker_with_warm_guests(0, small());
        let at_round = |rescore_count| {
            score(
                &ExecCandidate {
                    warm_guest_elsewhere: true,
                    rescore_count,
                    ..candidate(&plain)
                },
                &cold,
            )
            .vetoes
        };

        assert_eq!(at_round(0), [WARM_GUEST_ELSEWHERE]);
        assert!(at_round(EXEC_WARM_GUEST_HOLD_ROUNDS).is_empty());
    }

    #[test]
    fn a_warm_worker_or_a_job_without_a_warm_guest_elsewhere_is_never_held() {
        let plain = requirement(|_| {});
        let warm = worker_with_warm_guests(1, small());
        let elsewhere = ExecCandidate {
            warm_guest_elsewhere: true,
            ..candidate(&plain)
        };

        assert!(score(&elsewhere, &warm).vetoes.is_empty());
        assert!(
            score(&candidate(&plain), &ExecInventory::default())
                .vetoes
                .is_empty()
        );
    }

    #[test]
    fn the_breakdown_names_every_term_and_sums_to_the_total() {
        let plain = requirement(|_| {});
        let breakdown = score(&candidate(&plain), &worker_with_warm_guests(1, small()));

        assert_eq!(
            breakdown
                .rules
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [DEVICE_FIT, PROGRAM_CACHED, WAIT_TIME, WARM_GUEST]
        );
        assert_eq!(breakdown.rules.values().sum::<f64>(), breakdown.total);
    }
}
