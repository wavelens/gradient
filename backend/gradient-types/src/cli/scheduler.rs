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

    /// Seconds every member of a cluster attempt has to accept its assignment
    /// before the attempt is aborted and the cluster queued again.
    #[arg(
        long = "scheduler-cluster-prepare-timeout-secs",
        env = "GRADIENT_SCHEDULER_CLUSTER_PREPARE_TIMEOUT_SECS",
        default_value_t = 30
    )]
    pub cluster_prepare_timeout_secs: u64,

    /// Seconds a ready cluster waits for simultaneously idle slots before it
    /// reserves a target placement.
    #[arg(
        long = "scheduler-cluster-reserve-after-secs",
        env = "GRADIENT_SCHEDULER_CLUSTER_RESERVE_AFTER_SECS",
        default_value_t = 600
    )]
    pub cluster_reserve_after_secs: u64,

    /// Seconds a cluster reservation is held before it is released and planned again.
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
