/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::greater_than_zero;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct BuildArgs {
    #[arg(long = "build-max-attempts", env = "GRADIENT_BUILD_MAX_ATTEMPTS", value_parser = greater_than_zero::<u32>, default_value = "3")]
    pub max_attempts: u32,
    /// Free re-queues of a derivation available in a cache within one evaluation
    /// (attempts recorded `SubstituteUnavailable`, whichever failure produced them)
    /// before it is built like any other. A re-queue does not count as a build
    /// attempt: this is the only bound on that loop.
    #[arg(long = "build-substitute-miss-escalation-threshold", env = "GRADIENT_BUILD_SUBSTITUTE_MISS_ESCALATION_THRESHOLD", value_parser = greater_than_zero::<u32>, default_value = "2")]
    pub substitute_miss_escalation_threshold: u32,
    /// Max `InputsUnavailable` self-heal loops per build before the circuit
    /// breaker opens and the build fails fast instead of churning the cache.
    #[arg(long = "build-inputs-unavailable-max-loops", env = "GRADIENT_BUILD_INPUTS_UNAVAILABLE_MAX_LOOPS", value_parser = greater_than_zero::<u32>, default_value = "3")]
    pub inputs_unavailable_max_loops: u32,
    #[arg(
        long = "build-retry-backoff-secs",
        env = "GRADIENT_BUILD_RETRY_BACKOFF_SECS",
        default_value = "30"
    )]
    pub retry_backoff_secs: u64,
    #[arg(
        long = "build-default-timeout-secs",
        env = "GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS",
        default_value = "14400"
    )]
    pub default_timeout_secs: u64,
    #[arg(
        long = "build-default-max-silent-secs",
        env = "GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS",
        default_value = "3600"
    )]
    pub default_max_silent_secs: u64,
}

impl Default for BuildArgs {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            substitute_miss_escalation_threshold: 2,
            inputs_unavailable_max_loops: 3,
            retry_backoff_secs: 30,
            default_timeout_secs: 14400,
            default_max_silent_secs: 3600,
        }
    }
}
