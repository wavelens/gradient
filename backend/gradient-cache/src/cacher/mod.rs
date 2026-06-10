/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-side cache maintenance.
//!
//! The server no longer packs, compresses, or signs NARs - the worker does
//! all of that locally and uploads the compressed bytes with metadata and
//! per-cache signatures attached. This module only runs periodic cleanup /
//! GC passes against the cache's DB and NAR store.

mod cleanup;
mod deep_gc;
mod invalidate;
mod sign_sweep;

pub use self::deep_gc::{DeepGcReport, run_deep_gc};

pub use self::cleanup::{
    CleanupReport, cleanup_expired_upload_sessions, cleanup_old_evaluations,
    cleanup_orphaned_cache_files, cleanup_stale_build_request_blobs, cleanup_stale_cached_nars,
};
pub use self::invalidate::invalidate_cache_for_path;
pub use self::sign_sweep::sign_missing_signatures;

use gradient_core::ServerState;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{error, info};

pub async fn cache_loop(state: Arc<ServerState>) {
    let _guard = if state.config.registration.report_errors {
        Some(sentry::init(
            gradient_types::cli::effective_sentry_dsn(&state.config.registration).to_string(),
        ))
    } else {
        None
    };

    // No per-output work anymore - the worker uploads+signs. This loop only
    // runs maintenance. Tick every hour; nothing is latency-sensitive.
    let mut interval = time::interval(Duration::from_secs(3600));
    let cancel = state.shutdown.token();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("cache loop shutting down");
                return;
            }
            _ = interval.tick() => {}
        }

        match cleanup_orphaned_cache_files(Arc::clone(&state)).await {
            Ok(report) => info!(?report, "Cache cleanup completed"),
            Err(e) => error!(error = ?e, "Cache cleanup failed"),
        }
        if let Err(e) = cleanup_old_evaluations(Arc::clone(&state)).await {
            error!(error = ?e, "Evaluation GC failed");
        } else {
            info!("Evaluation GC completed successfully");
        }
        if let Err(e) = gradient_core::db::gc_orphan_derivations(
            &state.db(),
            state.config.storage.keep_orphan_derivations_hours,
        )
        .await
        {
            error!(error = ?e, "Derivation GC failed");
        } else {
            info!("Derivation GC completed successfully");
        }
        if state.config.storage.nar_ttl_hours > 0
            && let Err(e) = cleanup_stale_cached_nars(Arc::clone(&state)).await
        {
            error!(error = ?e, "NAR TTL GC failed");
        }
        if let Err(e) = gradient_core::ci::unpark_storage_full_all(
            &state.worker_db,
            state.config.storage.max_storage_gb,
        )
        .await
        {
            error!(error = ?e, "Failed to unpark storage-full evaluations after cleanup");
        }
        if let Err(e) = cleanup_stale_build_request_blobs(Arc::clone(&state)).await {
            error!(error = ?e, "Build-request blob GC failed");
        }
        if let Err(e) = cleanup_expired_upload_sessions(Arc::clone(&state)).await {
            error!(error = ?e, "Upload-session GC failed");
        }
    }
}

/// Periodic sweep that fills in `cached_path_signature` rows whose
/// `signature` column is still NULL. Ticks every 60 seconds.
pub async fn sign_sweep_loop(state: Arc<ServerState>) {
    let _guard = if state.config.registration.report_errors {
        Some(sentry::init(
            gradient_types::cli::effective_sentry_dsn(&state.config.registration).to_string(),
        ))
    } else {
        None
    };

    let mut interval = time::interval(Duration::from_secs(60));
    let cancel = state.shutdown.token();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("sign sweep loop shutting down");
                return;
            }
            _ = interval.tick() => {}
        }
        if let Err(e) = sign_missing_signatures(Arc::clone(&state)).await {
            error!(error = ?e, "Signature sweep failed");
        }
    }
}
