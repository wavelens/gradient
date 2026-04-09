/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod build;
mod status;

pub use build::{schedule_build, schedule_build_loop};

use gradient_core::types::ServerState;
use std::sync::Arc;

pub async fn start_builder(state: Arc<ServerState>) -> std::io::Result<()> {
    tokio::spawn(schedule_build_loop(Arc::clone(&state)));
    Ok(())
}
