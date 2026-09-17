/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cacher;
pub mod uploader;

use gradient_core::ServerState;
use std::sync::Arc;

pub async fn start_cache(state: Arc<ServerState>) -> std::io::Result<()> {
    for spec in cacher::child_specs(&state) {
        state
            .shutdown
            .supervise_now(spec)
            .await
            .map_err(std::io::Error::other)?;
    }
    state
        .shutdown
        .supervise_now(uploader::child_spec(&state))
        .await
        .map_err(std::io::Error::other)?;
    Ok(())
}

/// Pulls this crate into a binary that otherwise references nothing from it, so
/// the statements it declares with `gradient_db::sql!` reach the plan gate's
/// registry. A linker drops an rlib nothing mentions, registry entries included.
pub const fn link() {}
