/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU64;

use clap::Args;
use gradient_derive::config_group;

pub const DEFAULT_KEEP_EVALUATIONS: i32 = 30;

#[config_group("eval")]
#[derive(Args, Debug, Clone)]
pub struct EvalArgs {
    /// Maximum number of evaluations kept per task. It is the cap for the per-task setting, and
    /// new tasks start at the lower of 30 and this value. Set it to `0` to disable the limit.
    #[arg(default_value_t = DEFAULT_KEEP_EVALUATIONS as usize)]
    pub max_keep: usize,

    /// Total size in bytes of shared eval cache blobs. Older blobs leave the cache until the
    /// total is within this size.
    #[arg(default_value_t = 10 * 1024 * 1024 * 1024)]
    pub cache_max_total_bytes: u64,

    /// Days after which eval cache blobs leave the cache, regardless of the size limit.
    #[arg(default_value_t = 30)]
    pub cache_max_age_days: u64,

    /// Seconds between eval cache eviction passes.
    #[arg(default_value = "3600")]
    pub cache_sweep_interval_secs: NonZeroU64,
}

impl EvalArgs {
    /// The configured value saturates rather than wraps. A value past `i32::MAX` would
    /// otherwise become a negative ceiling rejecting everything.
    pub fn keep_evaluations_max(&self) -> Option<i32> {
        match self.max_keep {
            0 => None,
            max => Some(i32::try_from(max).unwrap_or(i32::MAX)),
        }
    }

    /// Creating a task above the ceiling would leave it unsaveable. The frontend sends the
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
