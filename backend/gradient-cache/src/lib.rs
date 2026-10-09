/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod blobs;
mod debug_index;
mod deep_gc;
mod eval_cache;
mod evaluations;
mod nar;
mod schedule;
mod signatures;
mod storage_migrations;
#[cfg(test)]
mod test_support;
mod units;

use gradient_core::ServerState;
use std::sync::Arc;

pub async fn start_cache(state: Arc<ServerState>) -> std::io::Result<()> {
    for spec in schedule::child_specs(&state) {
        state
            .shutdown
            .supervise_now(spec)
            .await
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

// Keep this rlib linked into binaries that use nothing else from it, so its sql! entries reach the SQL plan check.
pub const fn link() {}
