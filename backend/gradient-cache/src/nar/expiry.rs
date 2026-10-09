/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_graph::GcRequest;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use std::sync::Arc;
use tracing::{info, warn};

fn keep_hours(ttl_hours: u64, grace_hours: i64) -> i64 {
    (ttl_hours as i64).max(grace_hours)
}

pub(crate) async fn evict_expired_cached_paths(state: Arc<ServerState>) -> Result<u64> {
    let keep = keep_hours(
        state.config.gc.nar_ttl_hours,
        state.config.gc.nar_upload_grace_hours,
    );
    let scanned_at = now();
    let expired = gradient_db::maintenance::gc::expired_cached_paths(&state.worker_db, keep)
        .await
        .context("expired cached-path selection failed")?;
    if expired.is_empty() {
        return Ok(0);
    }

    let mut evicted = 0u64;
    for chunk in expired.chunks(gradient_db::IN_CHUNK_SIZE) {
        let report = state
            .graph
            .gc(GcRequest::Paths {
                hashes: chunk.to_vec(),
                scanned_at,
            })
            .await
            .context("retire expired paths")?;

        for hash in &report.retired {
            if let Err(e) = state.nar_storage.delete(hash).await {
                warn!(error = %e, %hash, "failed to remove an expired NAR");
            }
        }
        evicted += report.retired.len() as u64;
    }

    state
        .events
        .publish(gradient_types::events::cache::Changed {});
    if evicted > 0 {
        info!(evicted, "Evicted expired cached paths");
        state
            .record(Swept {
                pass: Pass::StaleCachedPaths,
                removed: evicted,
            })
            .await;
    }
    Ok(evicted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_bound_never_undercuts_the_upload_grace() {
        assert_eq!(keep_hours(0, 24), 24);
        assert_eq!(keep_hours(336, 24), 336);
    }
}
