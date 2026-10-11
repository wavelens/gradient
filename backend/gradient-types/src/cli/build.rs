/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::greater_than_zero;
use clap::Args;
use gradient_derive::config_group;

#[config_group("build")]
#[derive(Args, Debug, Clone)]
pub struct BuildArgs {
    /// Build or eval job attempts before a transient failure is permanent.
    #[arg(value_parser = greater_than_zero::<u32>, default_value = "3")]
    pub max_attempts: u32,
    /// Free re-queues of a derivation available in a cache within one evaluation, before
    /// building the derivation like any other. A re-queue does not count as a build attempt.
    /// This threshold is the only bound on that loop.
    #[arg(value_parser = greater_than_zero::<u32>, default_value = "2")]
    pub substitute_miss_escalation_threshold: u32,
    /// Times a build can retry after missing inputs before failing instead of retrying again.
    #[arg(value_parser = greater_than_zero::<u32>, default_value = "3")]
    pub inputs_unavailable_max_loops: u32,
    /// Seconds before retrying a transient build failure, doubled for every previous attempt.
    #[arg(default_value = "30")]
    pub retry_backoff_secs: u64,
    /// Build timeout in seconds for derivations without `timeout`. Set it to `0` to disable
    /// the timeout.
    #[arg(default_value = "14400")]
    pub default_timeout_secs: u64,
    /// Timeout in seconds without build output for derivations without `maxSilent`. Set it to
    /// `0` to disable the timeout.
    #[arg(default_value = "3600")]
    pub default_max_silent_secs: u64,
}
