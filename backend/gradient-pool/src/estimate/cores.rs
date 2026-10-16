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

pub fn choose_cores(
    worker: &WorkerMetricsView,
    max_cores: Option<u32>,
    unmeasured_mb: u64,
    need_at: impl Fn(u32) -> RamNeed,
) -> CoreChoice {
    let full = max_cores.map_or(worker.full_cores(), |cap| cap.min(worker.full_cores()));
    let choices: Vec<CoreChoice> = core_steps(full)
        .into_iter()
        .map(|cores| {
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
        let choice = choose_cores(&worker(128_000, 119_000, 0), None, 0, linear(2_000));
        assert_eq!((choice.cores, choice.fit), (43, RamFit::Fits));
    }

    #[test]
    fn a_build_waits_for_ram_instead_of_shrinking_when_no_step_fits_now() {
        let busy = worker(128_000, 1_000, 100_000);
        let choice = choose_cores(&busy, None, 0, linear(1_500));
        assert_eq!(
            (choice.cores, choice.fit),
            (64, RamFit::FitsOnceRamIsReleased)
        );
    }

    #[test]
    fn a_step_that_fits_now_wins_over_a_larger_one_after_a_wait() {
        let busy = worker(128_000, 50_000, 70_000);
        let choice = choose_cores(&busy, None, 0, linear(1_000));
        assert_eq!((choice.cores, choice.fit), (43, RamFit::Fits));
    }

    #[test]
    fn a_build_too_large_at_one_core_does_not_fit() {
        let choice = choose_cores(&worker(64_000, 64_000, 0), None, 0, |_| {
            RamNeed::Predicted(70_000)
        });
        assert_eq!((choice.cores, choice.fit), (1, RamFit::DoesNotFit));
    }

    #[test]
    fn the_cap_after_an_out_of_memory_kill_bounds_the_steps() {
        let choice = choose_cores(&worker(128_000, 128_000, 0), Some(29), 0, linear(10));
        assert_eq!(choice.cores, 29);
    }

    #[test]
    fn a_worker_without_any_core_count_still_gets_one_core() {
        let unknown = WorkerMetricsView::default();
        let choice = choose_cores(&unknown, None, 0, |_| RamNeed::Negligible);
        assert_eq!(choice.cores, 1);
    }
}
