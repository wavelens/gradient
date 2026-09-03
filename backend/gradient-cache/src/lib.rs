/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cacher;

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
    Ok(())
}
