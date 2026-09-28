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

    /// Persist runner-up scoring candidates on each `dispatched_job` row.
    #[arg(
        long = "scheduler-record-candidates",
        env = "GRADIENT_SCHEDULER_RECORD_CANDIDATES",
        default_value_t = false
    )]
    pub record_candidates: bool,

    /// Days to retain `dispatched_job` forensic rows. 0 = keep forever.
    #[arg(
        long = "scheduler-dispatch-retention-days",
        env = "GRADIENT_SCHEDULER_DISPATCH_RETENTION_DAYS",
        default_value_t = 30
    )]
    pub dispatch_retention_days: i64,
}

impl Default for SchedulerArgs {
    fn default() -> Self {
        Self {
            scoring_policy: "resource-aware".into(),
            record_candidates: false,
            dispatch_retention_days: 30,
        }
    }
}
