/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cacher;

use gradient_core::ServerState;
use std::sync::Arc;

pub async fn start_cache(state: Arc<ServerState>) -> std::io::Result<()> {
    let shutdown = state.shutdown.clone();
    shutdown.spawn(cacher::cache_loop(Arc::clone(&state)));
    shutdown.spawn(cacher::sign_sweep_loop(Arc::clone(&state)));
    shutdown.spawn(cacher::eval_cache_sweep_loop(Arc::clone(&state)));
    Ok(())
}
