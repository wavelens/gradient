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
    #[arg(value_parser = greater_than_zero::<u32>, default_value = "3")]
    pub max_attempts: u32,
    /// Free re-queues of a derivation available in a cache within one evaluation, before Gradient
    /// is building the derivation like any other. A re-queue is not counting as a build attempt.
    /// This threshold is the only bound on that loop.
    #[arg(value_parser = greater_than_zero::<u32>, default_value = "2")]
    pub substitute_miss_escalation_threshold: u32,
    /// Max `InputsUnavailable` self-heal loops per build. The circuit breaker is opening after this
    /// many loops and the build is failing fast instead of churning the cache.
    #[arg(value_parser = greater_than_zero::<u32>, default_value = "3")]
    pub inputs_unavailable_max_loops: u32,
    #[arg(default_value = "30")]
    pub retry_backoff_secs: u64,
    #[arg(default_value = "14400")]
    pub default_timeout_secs: u64,
    #[arg(default_value = "3600")]
    pub default_max_silent_secs: u64,
}
