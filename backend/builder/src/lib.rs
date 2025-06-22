/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod evaluator;
pub mod scheduler;

use core::types::ServerState;
use std::sync::Arc;

pub async fn start_builder(state: Arc<ServerState>) -> std::io::Result<()> {
    tokio::spawn(scheduler::schedule_evaluation_loop(Arc::clone(&state)));
    tokio::spawn(scheduler::schedule_build_loop(Arc::clone(&state)));
    Ok(())
}
