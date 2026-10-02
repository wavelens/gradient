/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct SchedulerArgs {
    /// Name of the scheduler scoring policy (`simple`, `resource-aware`).
    /// Unknown names fall back to `resource-aware`.
    #[arg(
        long = "scheduler-scoring-policy",
        env = "GRADIENT_SCHEDULER_SCORING_POLICY",
        default_value = "resource-aware"
    )]
    pub scoring_policy: String,

    /// Seconds for every member of a cluster job attempt to accept its assignment. An attempt not
    /// accepted by all members in time is aborted, and the cluster job is queued again.
    #[arg(
        long = "scheduler-cluster-prepare-timeout-secs",
        env = "GRADIENT_SCHEDULER_CLUSTER_PREPARE_TIMEOUT_SECS",
        default_value_t = 30
    )]
    pub cluster_prepare_timeout_secs: u64,

    /// Seconds a cluster job that can start is waiting for enough simultaneously idle workers
    /// before reserving a placement. Reserved workers are receiving no new single jobs until the
    /// cluster job is starting or the reservation is expiring.
    #[arg(
        long = "scheduler-cluster-reserve-after-secs",
        env = "GRADIENT_SCHEDULER_CLUSTER_RESERVE_AFTER_SECS",
        default_value_t = 600
    )]
    pub cluster_reserve_after_secs: u64,

    /// Seconds to hold a cluster job reservation before the scheduler is releasing the reservation
    /// and planning the cluster job again.
    #[arg(
        long = "scheduler-cluster-reserve-timeout-secs",
        env = "GRADIENT_SCHEDULER_CLUSTER_RESERVE_TIMEOUT_SECS",
        default_value_t = 1800
    )]
    pub cluster_reserve_timeout_secs: u64,
}

impl Default for SchedulerArgs {
    fn default() -> Self {
        Self {
            scoring_policy: "resource-aware".into(),
            cluster_prepare_timeout_secs: 30,
            cluster_reserve_after_secs: 600,
            cluster_reserve_timeout_secs: 1800,
        }
    }
}
