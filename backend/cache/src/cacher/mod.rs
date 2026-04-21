/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-side cache maintenance.
//!
//! The server no longer packs, compresses, or signs NARs — the worker does
//! all of that locally and uploads the compressed bytes with metadata and
//! per-cache signatures attached. This module only runs periodic cleanup /
//! GC passes against the cache's DB and NAR store.

mod cleanup;
mod invalidate;
mod sign_sweep;

pub use self::cleanup::{
    cleanup_old_evaluations, cleanup_orphaned_cache_files, cleanup_stale_cached_nars,
};
pub use self::invalidate::invalidate_cache_for_path;
pub use self::sign_sweep::sign_missing_signatures;

use core::types::*;
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

    // No per-output work anymore — the worker uploads+signs. This loop only
    // runs maintenance. Tick every hour; nothing is latency-sensitive.
    let mut interval = time::interval(Duration::from_secs(3600));

    loop {
        interval.tick().await;

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
    }
}

/// Periodic sweep that fills in `cached_path_signature` rows whose
/// `signature` column is still NULL. Ticks every 60 seconds.
pub async fn sign_sweep_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let mut interval = time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Err(e) = sign_missing_signatures(Arc::clone(&state)).await {
            error!(error = %e, "Signature sweep failed");
        }
    }
}
