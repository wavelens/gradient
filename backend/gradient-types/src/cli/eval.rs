/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

pub const DEFAULT_KEEP_EVALUATIONS: i32 = 30;

#[derive(Args, Debug, Clone)]
pub struct EvalArgs {
    /// Instance-wide maximum for a task's `keep_evaluations`. New tasks start at the lower of
    /// [`DEFAULT_KEEP_EVALUATIONS`] and this. `0` is disabling the cap.
    #[arg(
        long = "eval-max-keep",
        env = "GRADIENT_EVAL_MAX_KEEP",
        default_value_t = DEFAULT_KEEP_EVALUATIONS as usize
    )]
    pub max_keep: usize,

    /// Total byte cap for the fleet-shared eval-cache blobs. The periodic eviction sweep is
    /// dropping the oldest-`updated_at` rows until the surviving total is at or under this. The
    /// default is 10 GiB.
    #[arg(
        long = "eval-cache-max-total-bytes",
        env = "GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub cache_max_total_bytes: u64,

    /// Max age in days for an eval-cache blob. The sweep is evicting older blobs regardless of the
    /// size cap. The default is 30.
    #[arg(
        long = "eval-cache-max-age-days",
        env = "GRADIENT_EVAL_CACHE_MAX_AGE_DAYS",
        default_value_t = 30
    )]
    pub cache_max_age_days: u64,

    /// Interval in seconds between eval-cache eviction sweeps. The default is 3600.
    #[arg(
        long = "eval-cache-sweep-interval-secs",
        env = "GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub cache_sweep_interval_secs: u64,
}

impl Default for EvalArgs {
    fn default() -> Self {
        Self {
            max_keep: DEFAULT_KEEP_EVALUATIONS as usize,
            cache_max_total_bytes: 10 * 1024 * 1024 * 1024,
            cache_max_age_days: 30,
            cache_sweep_interval_secs: 3600,
        }
    }
}

impl EvalArgs {
    /// The configured value is saturating rather than wrapping. A value past `i32::MAX` would
    /// otherwise become a negative ceiling rejecting everything.
    pub fn keep_evaluations_max(&self) -> Option<i32> {
        match self.max_keep {
            0 => None,
            max => Some(i32::try_from(max).unwrap_or(i32::MAX)),
        }
    }

    /// Creating a task above the ceiling would leave it unsaveable. The frontend is sending the
    /// whole form back and the handed value would fail validation (#561).
    pub fn default_keep_evaluations(&self) -> i32 {
        match self.keep_evaluations_max() {
            Some(max) => DEFAULT_KEEP_EVALUATIONS.min(max),
            None => DEFAULT_KEEP_EVALUATIONS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(max_keep: usize) -> EvalArgs {
        EvalArgs {
            max_keep,
            ..Default::default()
        }
    }

    #[test]
    fn a_new_task_never_starts_above_the_maximum() {
        assert_eq!(args(3).default_keep_evaluations(), 3);
        assert_eq!(args(1).default_keep_evaluations(), 1);
    }

    #[test]
    fn a_higher_maximum_leaves_the_default_alone() {
        assert_eq!(
            args(100).default_keep_evaluations(),
            DEFAULT_KEEP_EVALUATIONS
        );
        assert_eq!(args(100).keep_evaluations_max(), Some(100));
    }

    #[test]
    fn zero_disables_the_cap() {
        assert_eq!(args(0).keep_evaluations_max(), None);
        assert_eq!(args(0).default_keep_evaluations(), DEFAULT_KEEP_EVALUATIONS);
    }

    #[test]
    fn an_out_of_range_maximum_saturates() {
        assert_eq!(args(usize::MAX).keep_evaluations_max(), Some(i32::MAX));
        assert_eq!(
            args(usize::MAX).default_keep_evaluations(),
            DEFAULT_KEEP_EVALUATIONS
        );
    }
}
