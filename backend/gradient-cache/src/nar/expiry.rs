/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::eviction::{CachedPathIndex, evict};
use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_graph::GcRequest;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use std::sync::Arc;
use tracing::info;

fn keep_hours(ttl_hours: u64, grace_hours: i64) -> i64 {
    (ttl_hours as i64).max(grace_hours)
}

struct ExpiredCachedPaths {
    state: Arc<ServerState>,
    keep_hours: i64,
    scanned_at: NaiveDateTime,
}

#[async_trait::async_trait]
impl CachedPathIndex for ExpiredCachedPaths {
    async fn expired(&self) -> Result<Vec<String>> {
        gradient_db::maintenance::gc::expired_cached_paths(&self.state.worker_db, self.keep_hours)
            .await
            .context("expired cached-path selection failed")
    }

    async fn retire(&self, keys: &[String]) -> Result<Vec<String>> {
        let report = self
            .state
            .graph
            .gc(GcRequest::Paths {
                hashes: keys.to_vec(),
                scanned_at: self.scanned_at,
            })
            .await
            .context("retire expired paths")?;
        Ok(report.retired)
    }
}

pub(crate) async fn evict_expired_cached_paths(state: Arc<ServerState>) -> Result<u64> {
    let index = ExpiredCachedPaths {
        keep_hours: keep_hours(
            state.config.gc.nar_ttl_hours,
            state.config.gc.nar_upload_grace_hours,
        ),
        scanned_at: now(),
        state: Arc::clone(&state),
    };
    let evicted = evict(&index, &state.nar_storage, gradient_db::IN_CHUNK_SIZE).await?;
    if evicted == 0 {
        return Ok(0);
    }

    info!(evicted, "Evicted expired cached paths");
    state
        .events
        .publish(gradient_types::events::cache::Changed {});
    state
        .record(Swept {
            pass: Pass::StaleCachedPaths,
            removed: evicted,
        })
        .await;
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
