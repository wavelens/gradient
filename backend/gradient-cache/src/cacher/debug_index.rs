/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Backfill for the DWARF build-id index.
//!
//! Uploads index their own NAR in place (`spawn_debug_index`), so this pass only
//! has to catch what that missed: paths cached before the index existed, and
//! walks lost to a restart or a storage hiccup. Each pass takes a bounded batch
//! of unscanned `-debug` outputs; the marker on `cached_path` means a NAR is read
//! at most once per distinct `file_hash`.

use gradient_core::ServerState;
use std::sync::Arc;
use tracing::{debug, warn};

/// Max NARs walked per pass. Each walk decompresses a whole debug output, so the
/// batch stays small; the remainder is picked up by the next tick.
const DEBUG_INDEX_BATCH: u64 = 32;

pub async fn index_pending_debug_info(state: Arc<ServerState>) -> anyhow::Result<()> {
    let pending = gradient_db::pending_debug_index(&state.worker_db, DEBUG_INDEX_BATCH).await?;
    if pending.is_empty() {
        return Ok(());
    }

    let mut build_ids = 0usize;
    for path in &pending {
        match gradient_db::index_cached_path(
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
