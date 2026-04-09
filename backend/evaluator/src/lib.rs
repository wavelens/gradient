/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod dependencies;
mod eval;
mod flake;
mod nix_eval;
mod scheduler;
pub mod worker;
pub mod worker_pool;

pub use eval::{EvaluationOutput, evaluate, evaluate_direct};
pub use scheduler::{schedule_evaluation, schedule_evaluation_loop};
pub use worker::run_eval_worker;
pub use worker_pool::WorkerPoolResolver;

use gradient_core::types::ServerState;
use std::sync::Arc;

pub async fn start_evaluator(state: Arc<ServerState>) -> std::io::Result<()> {
    tokio::spawn(scheduler::schedule_evaluation_loop(state));
    Ok(())
}
