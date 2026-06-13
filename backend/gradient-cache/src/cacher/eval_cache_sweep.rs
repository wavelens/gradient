/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Periodic eviction of fleet-shared eval-cache blobs.
//!
//! Bounds the `eval_cache_store` table (and its object-storage blobs) by age
//! and by total size: rows older than `max_age` go first, then - oldest
//! `updated_at` first - enough additional rows to bring the surviving total
//! `size_bytes` at or under the configured cap (#386).

use chrono::{Duration, NaiveDateTime};
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Rows are `(id, size_bytes, updated_at)`. Returns the ids to evict: every row
/// older than `max_age`, plus - oldest-`updated_at` first - enough additional
/// rows to bring the surviving total `size_bytes` at or under `max_total_bytes`.
fn select_evictions(
    rows: &[(EvalCacheStoreId, i64, NaiveDateTime)],
    max_total_bytes: u64,
    max_age: Duration,
    now: NaiveDateTime,
) -> Vec<EvalCacheStoreId> {
    let cutoff = now - max_age;

    let mut by_age: Vec<&(EvalCacheStoreId, i64, NaiveDateTime)> = rows.iter().collect();
    by_age.sort_by_key(|(_, _, updated_at)| *updated_at);

    let mut evicted: Vec<EvalCacheStoreId> = Vec::new();
    let mut surviving_total: u64 = 0;
    let mut survivors: Vec<&(EvalCacheStoreId, i64, NaiveDateTime)> = Vec::new();

    for row in &by_age {
        if row.2 < cutoff {
            evicted.push(row.0);
        } else {
            surviving_total = surviving_total.saturating_add(row.1.max(0) as u64);
            survivors.push(row);
        }
    }

    for row in survivors {
        if surviving_total <= max_total_bytes {
            break;
        }

        evicted.push(row.0);
        surviving_total = surviving_total.saturating_sub(row.1.max(0) as u64);
    }

    evicted
}

/// One sweep pass: load every `eval_cache_store` row, evict the ones selected
/// by [`select_evictions`], deleting the blob (best-effort) then the DB row.
/// Errors on a single row are logged and never abort the pass.
pub async fn evict_eval_cache(state: Arc<ServerState>) -> anyhow::Result<()> {
    let cfg = &state.config.storage;

    let rows: Vec<(EvalCacheStoreId, i64, NaiveDateTime)> = EEvalCacheStore::find()
        .select_only()
        .column(CEvalCacheStore::Id)
        .column(CEvalCacheStore::SizeBytes)
        .column(CEvalCacheStore::UpdatedAt)
        .into_tuple()
        .all(&state.worker_db)
        .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let victims = select_evictions(
        &rows,
        cfg.eval_cache_max_total_bytes,
        Duration::days(cfg.eval_cache_max_age_days as i64),
        gradient_types::now(),
    );

    if victims.is_empty() {
        return Ok(());
    }

    let models = EEvalCacheStore::find()
        .filter(CEvalCacheStore::Id.is_in(victims))
        .all(&state.worker_db)
        .await?;

    let mut freed: u64 = 0;
    let mut evicted = 0usize;
    for row in models {
        if let Err(e) = state.nar_storage.delete_eval_cache(&row.fingerprint).await {
            warn!(fingerprint = %row.fingerprint, error = ?e, "eval-cache sweep: blob delete failed");
        }

        match EEvalCacheStore::delete_by_id(row.id).exec(&state.worker_db).await {
            Ok(_) => {
                freed = freed.saturating_add(row.size_bytes.max(0) as u64);
                evicted += 1;
            }
            Err(e) => {
                error!(id = %row.id, error = ?e, "eval-cache sweep: row delete failed");
            }
        }
    }

    if evicted > 0 {
        info!(evicted, freed_bytes = freed, "eval-cache sweep: blobs evicted");
    }

    Ok(())
}

/// Periodic eval-cache eviction sweep; interval from
/// `storage.eval_cache_sweep_interval_secs`.
pub async fn eval_cache_sweep_loop(state: Arc<ServerState>) {
    let _guard = if state.config.registration.report_errors {
        Some(sentry::init(
            gradient_types::cli::effective_sentry_dsn(&state.config.registration).to_string(),
        ))
    } else {
        None
    };

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        state.config.storage.eval_cache_sweep_interval_secs.max(1),
    ));
    let cancel = state.shutdown.token();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("eval-cache sweep loop shutting down");
                return;
            }
            _ = interval.tick() => {}
        }

        if let Err(e) = evict_eval_cache(Arc::clone(&state)).await {
            error!(error = ?e, "eval-cache sweep failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> EvalCacheStoreId {
        EvalCacheStoreId::new(uuid::Uuid::from_u128(n))
    }

    fn ts(secs: i64) -> NaiveDateTime {
        chrono::DateTime::from_timestamp(secs, 0).unwrap().naive_utc()
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn empty_input_evicts_nothing() {
        let out = select_evictions(&[], 1024, Duration::days(30), ts(NOW));
        assert!(out.is_empty());
    }

    #[test]
    fn under_budget_and_fresh_evicts_nothing() {
        let rows = vec![
            (id(1), 100, ts(NOW - 10)),
            (id(2), 200, ts(NOW - 20)),
        ];
        let out = select_evictions(&rows, 1024, Duration::days(30), ts(NOW));
        assert!(out.is_empty());
    }

    #[test]
    fn aged_row_evicted_regardless_of_size() {
        let day = 86_400;
        let rows = vec![
            (id(1), 1, ts(NOW - 40 * day)),
            (id(2), 999, ts(NOW - 10)),
        ];
        let out = select_evictions(&rows, u64::MAX, Duration::days(30), ts(NOW));
        assert_eq!(out, vec![id(1)]);
    }

    #[test]
    fn over_cap_evicts_oldest_first_until_under() {
        let rows = vec![
            (id(1), 400, ts(NOW - 30)),
            (id(2), 400, ts(NOW - 20)),
            (id(3), 400, ts(NOW - 10)),
        ];
        let out = select_evictions(&rows, 800, Duration::days(30), ts(NOW));
        assert_eq!(out, vec![id(1)], "evict oldest until surviving total <= cap");
    }

    #[test]
    fn age_and_size_combine_without_double_counting() {
        let day = 86_400;
        let rows = vec![
            (id(1), 500, ts(NOW - 40 * day)),
            (id(2), 500, ts(NOW - 30)),
            (id(3), 500, ts(NOW - 20)),
            (id(4), 500, ts(NOW - 10)),
        ];
        let out = select_evictions(&rows, 800, Duration::days(30), ts(NOW));

        assert!(out.contains(&id(1)), "aged row evicted");
        assert!(out.contains(&id(2)), "oldest surviving evicted for size");
        assert_eq!(out.len(), 2, "no id counted twice; stop once under cap");
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(unique.len(), out.len(), "no duplicate ids");
    }
}
