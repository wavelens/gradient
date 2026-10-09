/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod nar;

#[cfg(feature = "server")]
mod blobs;
#[cfg(feature = "server")]
mod debug_index;
#[cfg(feature = "server")]
mod deep_gc;
#[cfg(feature = "server")]
mod eval_cache;
#[cfg(feature = "server")]
mod evaluations;
#[cfg(feature = "server")]
mod schedule;
#[cfg(feature = "server")]
mod signatures;
#[cfg(feature = "server")]
mod storage_migrations;
#[cfg(all(test, feature = "server"))]
mod test_support;
#[cfg(feature = "server")]
mod units;

#[cfg(feature = "server")]
pub async fn start_cache(state: std::sync::Arc<gradient_core::ServerState>) -> std::io::Result<()> {
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
#[cfg(feature = "server")]
pub const fn link() {}
