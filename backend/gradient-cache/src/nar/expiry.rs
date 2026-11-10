/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::eviction::{CachedPathIndex, evict};
use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use gradient_db::{DbContext, WorkerDb};
use gradient_graph::{GcRequest, Graph};
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::info;

fn keep_hours(ttl_hours: u64, grace_hours: i64) -> i64 {
    (ttl_hours as i64).max(grace_hours)
}

struct ExpiredCachedPaths<'a> {
    db: &'a WorkerDb,
    graph: &'a Graph,
    unused_since: NaiveDateTime,
    scanned_at: NaiveDateTime,
    deleted_claim_row_count: AtomicU64,
}

#[async_trait::async_trait]
impl CachedPathIndex for ExpiredCachedPaths<'_> {
    async fn expired(&self) -> Result<Vec<String>> {
        gradient_db::maintenance::gc::scan_deletable_path_hashes(self.db, self.unused_since)
            .await
            .context("select expired cached paths")
    }

    async fn filter_nar_hashes_with_cached_path_again(
        &self,
        keys: &[String],
    ) -> Result<HashSet<String>> {
        if keys.is_empty() {
            return Ok(HashSet::new());
        }
        ECachedPath::find()
            .select_only()
            .column(CCachedPath::Hash)
            .filter(CCachedPath::Hash.is_in(keys.iter().cloned()))
            .into_tuple::<String>()
            .all(self.db)
            .await
            .map(|hashes| hashes.into_iter().collect())
            .context("Search for cache paths that were uploaded again")
    }

    async fn retire(&self, keys: &[String]) -> Result<Vec<String>> {
        let report = self
            .graph
            .gc(GcRequest::UnusedCacheClaims {
                hashes: keys.to_vec(),
                unused_since: self.unused_since,
                scanned_at: self.scanned_at,
            })
            .await
            .context("retire expired paths")?;
        self.deleted_claim_row_count
            .fetch_add(report.deleted_claim_row_count, Ordering::Relaxed);
        Ok(report.retired)
    }
}

pub(crate) async fn evict_expired_cached_paths(db: &DbContext, graph: &Graph) -> Result<u64> {
    let scanned_at = now();
    let keep = keep_hours(
        db.config.gc.nar_ttl_hours,
        i64::from(db.config.gc.nar_upload_grace_hours),
    );
    let index = ExpiredCachedPaths {
        unused_since: scanned_at - chrono::Duration::hours(keep),
        scanned_at,
        db: &db.worker_db,
        graph,
        deleted_claim_row_count: AtomicU64::new(0),
    };
    let evicted = evict(&index, &db.storage.nar_storage, gradient_db::IN_CHUNK_SIZE).await?;
    let deleted_claim_row_count = index.deleted_claim_row_count.into_inner();
    if deleted_claim_row_count == 0 && evicted == 0 {
        return Ok(0);
    }

    info!(
        evicted,
        deleted_claim_row_count, "Evicted expired cached paths"
    );
    db.events.publish(gradient_types::events::cache::Changed {});
    if evicted > 0 {
        db.record(Swept {
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
