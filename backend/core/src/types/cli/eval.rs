/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::input::greater_than_zero;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct EvalArgs {
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_EVALUATIONS", value_parser = greater_than_zero::<usize>, default_value = "10")]
    pub max_concurrent_evaluations: usize,
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_BUILDS", value_parser = greater_than_zero::<usize>, default_value = "1000")]
    pub max_concurrent_builds: usize,
    #[arg(long, env = "GRADIENT_EVALUATION_TIMEOUT", value_parser = greater_than_zero::<i64>, default_value = "10")]
    pub evaluation_timeout: i64,
    /// Number of long-lived Nix evaluator worker subprocesses to keep around.
    /// Each worker hosts one persistent embedded `NixEvaluator`, paying the
    /// libnix init cost only once. Must be at least `1`: in-process evaluation
    /// is unsafe because the Nix C API `EvalState` is not thread-safe and the
    /// embedded Boehm GC conflicts with Tokio's signal handling.
    #[arg(long, env = "GRADIENT_EVAL_WORKERS", value_parser = greater_than_zero::<usize>, default_value = "1")]
    pub eval_workers: usize,
    /// Recycle an eval-worker subprocess after it has served this many
    /// `list` / `resolve` calls. Nix's Boehm GC never releases memory
    /// back to the OS, so long-lived workers grow monotonically; this
    /// cap bounds RSS growth by forcing a respawn. Set to 0 to disable.
    #[arg(long, env = "GRADIENT_MAX_EVALUATIONS_PER_WORKER", default_value = "1")]
    pub max_evaluations_per_worker: usize,
}

impl Default for EvalArgs {
    fn default() -> Self {
        Self {
            max_concurrent_evaluations: 10,
            max_concurrent_builds: 1000,
            evaluation_timeout: 10,
            eval_workers: 1,
            max_evaluations_per_worker: 1,
        }
    }
}
