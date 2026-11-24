/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::score::{RamFit, RamNeed, WorkerMetricsView};

pub fn fewer_cores(cores: u32) -> u32 {
    cores.saturating_sub(1).min((2 * cores).div_ceil(3)).max(1)
}

pub fn core_steps(full: u32) -> Vec<u32> {
    let mut steps = vec![full.max(1)];
    while let Some(&last) = steps.last().filter(|c| **c > 1) {
        steps.push(fewer_cores(last));
    }
    steps
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreChoice {
    pub cores: u32,
    pub need: RamNeed,
    pub fit: RamFit,
}

/// Builds keep the largest core step that can fit in the total RAM of this worker or in the total
/// RAM of the largest worker the project can use. Builds with predicted RAM above the total RAM of
/// this worker wait for that larger worker at that step. The build can wait for that RAM at a step
/// that can fit after release of the RAM reserved for assigned jobs. Only a worker with no
/// reservations can shrink the build to the largest step that can fit right now.
///
/// Builds that match no step in the estimate get RAM proportional to core count. That RAM can pick
/// the starting point. OOM kills lower the step from there. Upper limits for builds that stay too
/// large even on all workers the project can use come from the total RAM of the largest worker.
/// Smaller workers leave the build to that one machine.
pub fn choose_cores(
    worker: &WorkerMetricsView,
    max_cores: Option<u32>,
    unmeasured_mb: u64,
    largest_usable_worker_ram_mb: Option<u64>,
    need_at: impl Fn(u32) -> RamNeed,
) -> CoreChoice {
    let full = max_cores.map_or(worker.full_cores(), |cap| cap.min(worker.full_cores()));
    let steps = core_steps(full);
    let core_choice_for_core_count = |cores: u32| {
        let need = need_at(cores);
        CoreChoice {
            cores,
            need,
            fit: worker.ram_fit(need.reserved_mb(unmeasured_mb)),
        }
    };
    let ceiling_mb = worker
        .ram_total_mb
        .max(largest_usable_worker_ram_mb.unwrap_or(0));
    let target = steps.iter().position(|&cores| {
        ceiling_mb == 0 || need_at(cores).reserved_mb(unmeasured_mb) <= ceiling_mb
    });
    if let Some(target) = target {
        let choice = core_choice_for_core_count(steps[target]);
        if worker.ram_total_mb > 0 && choice.need.reserved_mb(unmeasured_mb) > worker.ram_total_mb {
            return CoreChoice {
                fit: RamFit::WaitsForLargerWorker,
                ..choice
            };
        }
        if choice.fit != RamFit::DoesNotFit {
            return choice;
        }
        let smaller: Vec<CoreChoice> = steps[target + 1..]
            .iter()
            .map(|&cores| core_choice_for_core_count(cores))
            .collect();
        return smaller
            .iter()
            .find(|c| c.fit == RamFit::Fits)
            .or(smaller.last())
            .copied()
            .unwrap_or(choice);
    }
    let estimated = largest_fitting(worker, &steps, unmeasured_mb, &need_at);
    let RamNeed::Predicted(at_full) = need_at(full) else {
        return estimated;
    };
    let proportional = |at_full: u64| {
        largest_fitting(worker, &steps, unmeasured_mb, |cores| {
            RamNeed::Predicted((at_full * u64::from(cores)).div_ceil(u64::from(full)))
        })
    };
    let scaled = proportional(at_full);
    match largest_usable_worker_ram_mb {
        _ if scaled.fit != RamFit::DoesNotFit => scaled,
        Some(largest) if worker.ram_total_mb >= largest => {
            proportional(at_full.min(worker.ram_total_mb))
        }
        Some(_) => CoreChoice {
            fit: RamFit::WaitsForLargerWorker,
            ..scaled
        },
        None => scaled,
    }
}

fn largest_fitting(
    worker: &WorkerMetricsView,
    steps: &[u32],
    unmeasured_mb: u64,
    need_at: impl Fn(u32) -> RamNeed,
) -> CoreChoice {
    let choices: Vec<CoreChoice> = steps
        .iter()
        .map(|&cores| {
            let need = need_at(cores);
            CoreChoice {
                cores,
                need,
                fit: worker.ram_fit(need.reserved_mb(unmeasured_mb)),
            }
        })
        .collect();
    [RamFit::Fits, RamFit::FitsOnceRamIsReleased]
        .into_iter()
        .find_map(|fit| choices.iter().find(|c| c.fit == fit))
        .or(choices.last())
        .copied()
        .expect("core_steps is never empty")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(total_mb: u64, free_mb: u64, reserved_mb: u64) -> WorkerMetricsView {
        WorkerMetricsView {
            cpu_count: 64,
            ram_total_mb: total_mb,
            ram_free_mb: Some(free_mb),
            ram_reserved_mb: reserved_mb,
            ..WorkerMetricsView::default()
        }
    }

    fn linear(mb_per_core: u64) -> impl Fn(u32) -> RamNeed {
        move |cores| RamNeed::Predicted(mb_per_core * u64::from(cores))
    }

    #[test]
    fn each_step_takes_a_third_off_and_ends_at_one_core() {
        assert_eq!(
            core_steps(64),
            vec![64, 43, 29, 20, 14, 10, 7, 5, 4, 3, 2, 1]
        );
        assert_eq!((fewer_cores(2), fewer_cores(1)), (1, 1));
    }

    #[test]
    fn the_largest_step_that_fits_now_is_chosen() {
        let choice = choose_cores(&worker(128_000, 119_000, 0), None, 0, None, linear(2_000));
        assert_eq!((choice.cores, choice.fit), (43, RamFit::Fits));
    }

    #[test]
    fn a_build_waits_for_ram_instead_of_shrinking_when_no_step_fits_now() {
        let busy = worker(128_000, 1_000, 100_000);
        let choice = choose_cores(&busy, None, 0, None, linear(1_500));
        assert_eq!(
            (choice.cores, choice.fit),
            (64, RamFit::FitsOnceRamIsReleased)
        );
    }

    #[test]
    fn largest_step_that_fits_worker_total_ram_waits_for_reserved_ram_instead_of_starting_with_fewer_cores()
     {
        let busy = worker(128_000, 50_000, 70_000);
        let choice = choose_cores(&busy, None, 0, None, linear(1_000));
        assert_eq!(
            (choice.cores, choice.fit),
            (64, RamFit::FitsOnceRamIsReleased)
        );
    }

    fn predicted_build_ram_from_four_core_ram_scaled_by_sqrt_core_count(
        mb_at_four_cores: u64,
    ) -> impl Fn(u32) -> RamNeed {
        move |cores| {
            RamNeed::Predicted(mb_at_four_cores * (u64::from(cores) * 1_000_000).isqrt() / 2_000)
        }
    }

    #[test]
    fn test_eight_core_build_waits_for_reserved_ram_instead_of_shrinking_to_four_cores() {
        let busy = WorkerMetricsView {
            cpu_count: 12,
            ram_total_mb: 32_087,
            ram_free_mb: Some(21_900),
            ram_reserved_mb: 145,
            ..WorkerMetricsView::default()
        };
        for largest_usable_worker_ram_mb in [Some(32_087), None] {
            let choice = choose_cores(
                &busy,
                None,
                0,
                largest_usable_worker_ram_mb,
                predicted_build_ram_from_four_core_ram_scaled_by_sqrt_core_count(20_400),
            );
            assert_eq!(
                (choice.cores, choice.fit),
                (8, RamFit::FitsOnceRamIsReleased)
            );
        }
    }

    #[test]
    fn build_waits_for_larger_worker_at_largest_core_step() {
        let choice = choose_cores(
            &worker(32_000, 32_000, 0),
            None,
            0,
            Some(64_000),
            linear(800),
        );
        assert_eq!(
            (choice.cores, choice.fit),
            (64, RamFit::WaitsForLargerWorker)
        );
    }

    #[test]
    fn test_worker_with_too_little_free_ram_and_no_released_reservations_shrinks_build_to_largest_fitting_core_step()
     {
        let choice = choose_cores(&worker(128_000, 50_000, 0), None, 0, None, linear(1_000));
        assert_eq!((choice.cores, choice.fit), (43, RamFit::Fits));
    }

    #[test]
    fn a_build_too_large_at_every_step_assumes_ram_proportional_to_cores() {
        let choice = choose_cores(&worker(60_000, 60_000, 0), None, 0, None, |_| {
            RamNeed::Predicted(100_000)
        });
        assert_eq!((choice.cores, choice.fit), (29, RamFit::Fits));
        assert_eq!(choice.need, RamNeed::Predicted(45_313));
    }

    #[test]
    fn a_build_too_large_even_at_one_proportional_core_does_not_fit() {
        let choice = choose_cores(&worker(1_000, 1_000, 0), None, 0, None, |_| {
            RamNeed::Predicted(128_000)
        });
        assert_eq!((choice.cores, choice.fit), (1, RamFit::DoesNotFit));
    }
    #[test]
    fn the_cap_after_an_out_of_memory_kill_bounds_the_steps() {
        let choice = choose_cores(&worker(128_000, 128_000, 0), Some(29), 0, None, linear(10));
        assert_eq!(choice.cores, 29);
    }

    #[test]
    fn a_worker_without_any_core_count_still_gets_one_core() {
        let unknown = WorkerMetricsView::default();
        let choice = choose_cores(&unknown, None, 0, None, |_| RamNeed::Negligible);
        assert_eq!(choice.cores, 1);
    }

    #[test]
    fn test_build_ram_above_largest_worker_caps_ram_requirement_and_fits_at_reduced_core_count() {
        let choice = choose_cores(&worker(1_000, 800, 0), None, 0, Some(1_000), |_| {
            RamNeed::Predicted(128_000)
        });
        assert_eq!((choice.cores, choice.fit), (43, RamFit::Fits));
        assert_eq!(choice.need, RamNeed::Predicted(672));
    }

    #[test]
    fn test_build_too_large_for_all_core_counts_on_smaller_than_largest_usable_worker_gets_no_cap_and_does_not_fit()
     {
        let choice = choose_cores(&worker(1_000, 1_000, 0), None, 0, Some(2_000), |_| {
            RamNeed::Predicted(128_000)
        });
        assert_eq!(
            (choice.cores, choice.fit),
            (1, RamFit::WaitsForLargerWorker)
        );
    }

    #[test]
    fn build_that_fits_by_estimate_gets_same_core_count_with_or_without_largest_usable_worker_ram()
    {
        assert_eq!(
            choose_cores(
                &worker(128_000, 119_000, 0),
                None,
                0,
                Some(128_000),
                linear(2_000)
            ),
            choose_cores(&worker(128_000, 119_000, 0), None, 0, None, linear(2_000))
        );
    }
}
