/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ServerState;
use std::sync::Arc;
use tracing::{debug, warn};

const DEBUG_INDEX_BATCH: u64 = 32;

pub async fn index_pending_debug_info(state: Arc<ServerState>) -> anyhow::Result<()> {
    let pending =
        gradient_db::caches::debug_info::pending_debug_index(&state.worker_db, DEBUG_INDEX_BATCH)
            .await?;
    if pending.is_empty() {
        return Ok(());
    }

    let mut build_ids = 0usize;
    for path in &pending {
        match gradient_db::caches::debug_info::index_cached_path(
            &state.worker_db,
            &state.nar_storage,
            path.id,
            &path.hash,
        )
        .await
        {
            Ok(count) => build_ids += count,
            Err(e) => warn!(hash = %path.hash, error = %e, "debug index backfill failed"),
        }
    }

    debug!(
        scanned = pending.len(),
        build_ids, "debug-info backfill pass complete"
    );
    Ok(())
}
