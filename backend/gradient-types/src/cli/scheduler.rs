/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU64;

use clap::{Args, ValueEnum};
use gradient_derive::config_group;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringPolicy {
    Simple,
    ResourceAware,
}

#[config_group("scheduler")]
#[derive(Args, Debug, Clone)]
pub struct SchedulerArgs {
    /// Policy ranking queued jobs for a requesting worker. Factors of `simple`: path
    /// availability, NAR size, dependency count, waiting time, builtins and fetch worker
    /// reservation. Factors of `resource-aware`: the same plus memory fit, worker saturation,
    /// CPU, disk and network affinity and `preferLocalBuild`.
    #[arg(value_enum, default_value_t = ScoringPolicy::ResourceAware)]
    pub scoring_policy: ScoringPolicy,

    /// Seconds for every member of a cluster job attempt to accept its assignment. Attempts not
    /// accepted by all members in time abort, and their cluster jobs return to the queue.
    #[arg(default_value = "30")]
    pub cluster_prepare_timeout_secs: NonZeroU64,

    /// Seconds cluster jobs that can start wait for enough simultaneously idle workers before
    /// reserving a placement. Reserved workers receive no new single jobs until the cluster
    /// job's start or the reservation's expiry.
    #[arg(default_value = "600")]
    pub cluster_reserve_after_secs: NonZeroU64,

    /// Seconds to hold a cluster job reservation before releasing it and planning the cluster
    /// job again.
    #[arg(default_value = "1800")]
    pub cluster_reserve_timeout_secs: NonZeroU64,
}
