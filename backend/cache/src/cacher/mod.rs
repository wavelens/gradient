/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod cleanup;
mod invalidate;
mod output;
mod signing;

pub use self::cleanup::{
    cleanup_old_evaluations, cleanup_orphaned_cache_files, cleanup_stale_cached_nars,
};
pub use self::invalidate::invalidate_cache_for_path;
pub use self::output::cache_derivation_output;
pub use self::signing::sign_derivation_output;

use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{error, info};

pub async fn cache_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let concurrency = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut interval = time::interval(Duration::from_secs(5));
    let mut cleanup_counter = 0;
    const CLEANUP_INTERVAL: u32 = 720;

    loop {
        let outputs = get_next_uncached_derivation_outputs(Arc::clone(&state), concurrency).await;

        if outputs.is_empty() {
            interval.tick().await;

            cleanup_counter += 1;
            if cleanup_counter >= CLEANUP_INTERVAL {
                cleanup_counter = 0;
                if let Err(e) = cleanup_orphaned_cache_files(Arc::clone(&state)).await {
                    error!(error = %e, "Cache cleanup failed");
                } else {
                    info!("Cache cleanup completed successfully");
                }
                if let Err(e) = cleanup_old_evaluations(Arc::clone(&state)).await {
                    error!(error = %e, "Evaluation GC failed");
                } else {
                    info!("Evaluation GC completed successfully");
                }
                if let Err(e) = core::db::gc_orphan_derivations(
                    Arc::clone(&state),
                    state.cli.keep_orphan_derivations_hours,
                )
                .await
                {
                    error!(error = %e, "Derivation GC failed");
                } else {
                    info!("Derivation GC completed successfully");
                }
                if state.cli.nar_ttl_hours > 0
                    && let Err(e) = cleanup_stale_cached_nars(Arc::clone(&state)).await
                {
                    error!(error = %e, "NAR TTL GC failed");
                }
                if let Err(e) = self::cleanup::validate_cached_outputs(Arc::clone(&state)).await {
                    error!(error = %e, "Cached output validation failed");
                }
                if let Err(e) = self::cleanup::sign_missing_signatures(Arc::clone(&state)).await {
                    error!(error = %e, "Missing signature signing failed");
                }
            }
        } else {
            let tasks: Vec<_> = outputs
                .into_iter()
                .map(|output| {
                    let s = Arc::clone(&state);
                    tokio::spawn(async move { cache_derivation_output(s, output).await })
                })
                .collect();
            for task in tasks {
                let _ = task.await;
            }
        }
    }
}

async fn get_next_uncached_derivation_outputs(
    state: Arc<ServerState>,
    limit: usize,
) -> Vec<MDerivationOutput> {
    EDerivationOutput::find()
        .filter(CDerivationOutput::IsCached.eq(false))
        .order_by_asc(CDerivationOutput::CreatedAt)
        .limit(limit as u64)
        .all(&state.db)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "Failed to query next derivation outputs");
            vec![]
        })
}
