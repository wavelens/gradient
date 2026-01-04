/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cacher;

use core::types::ServerState;
use std::sync::Arc;

pub async fn start_cache(state: Arc<ServerState>) -> std::io::Result<()> {
    tokio::spawn(cacher::cache_loop(Arc::clone(&state)));
    Ok(())
}
