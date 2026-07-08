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
mod eval_cache_sweep;
mod invalidate;
mod sign_sweep;
#[cfg(test)]
pub(crate) mod test_support;

pub use self::deep_gc::{DeepGcReport, run_deep_gc};
pub use self::eval_cache_sweep::{eval_cache_sweep_loop, evict_eval_cache};

pub use self::cleanup::{
    CleanupReport, cleanup_expired_upload_sessions, cleanup_old_evaluations,
    cleanup_orphaned_cache_files, cleanup_stale_build_request_blobs, cleanup_stale_cached_nars,
};
pub use self::invalidate::invalidate_cache_for_path;
pub use self::sign_sweep::sign_missing_signatures;

use futures::future::BoxFuture;
use gradient_core::ServerState;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time;
use tracing::{error, info};

/// One periodic background pass: a name (for logs), a tick interval, and the
/// async fn to run. Registered in [`sweeps`] and driven by [`run_sweep`].
struct Sweep {
    name: &'static str,
    interval_secs: u64,
    run: Box<dyn Fn(Arc<ServerState>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
}

impl Sweep {
    fn new<F, Fut>(name: &'static str, interval_secs: u64, run: F) -> Self
    where
        F: Fn(Arc<ServerState>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Sweep {
            name,
            interval_secs,
            run: Box::new(move |state| Box::pin(run(state))),
        }
    }
}

/// The registered sweeps. "cache-maintenance" bundles the 9 order-sensitive
/// GC/reconcile steps that used to live in the monolithic `cache_loop`
/// (orphan-files, eval GC, derivation GC, NAR TTL, demote-unbacked,
/// unpark-storage-full, build-request blobs, upload sessions, partial-store
/// GC); "sign-sweep" is the signature backfill. Each runs on its own
/// interval and its own spawned loop.
fn sweeps(state: &ServerState) -> Vec<Sweep> {
    vec![
        Sweep::new(
            "cache-maintenance",
            state.config.storage.cache_maintenance_interval_secs.max(1),
            |state| Box::pin(run_cache_maintenance(state)),
        ),
        Sweep::new(
            "sign-sweep",
            state.config.storage.sign_sweep_interval_secs.max(1),
            |state| Box::pin(sign_missing_signatures(state)),
        ),
    ]
}

/// Drives one [`Sweep`] on its own interval until shutdown. Errors from a
/// single run are logged and never abort the loop.
async fn run_sweep(state: Arc<ServerState>, sweep: Sweep) {
    let _guard = if state.config.registration.report_errors {
        Some(sentry::init(
            gradient_types::cli::effective_sentry_dsn(&state.config.registration).to_string(),
        ))
    } else {
        None
    };

    let mut interval = time::interval(Duration::from_secs(sweep.interval_secs));
    let cancel = state.shutdown.token();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!(sweep = sweep.name, "sweep loop shutting down");
                return;
            }
            _ = interval.tick() => {}
        }

        let started = Instant::now();
        match (sweep.run)(Arc::clone(&state)).await {
            Ok(()) => info!(
                sweep = sweep.name,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "sweep completed"
            ),
            Err(e) => error!(
                sweep = sweep.name,
                elapsed_ms = started.elapsed().as_millis() as u64,
                error = ?e,
                "sweep failed"
            ),
        }
    }
}

/// Spawns every registered sweep as its own long-lived task.
pub fn spawn_sweeps(state: &Arc<ServerState>) {
    for sweep in sweeps(state) {
        state.shutdown.spawn(run_sweep(Arc::clone(state), sweep));
    }
}

/// The 9 order-sensitive cache-maintenance steps, run sequentially every
/// `cache_maintenance_interval_secs`. No per-output work here - the worker
/// uploads+signs; this is GC and self-heal reconciliation only.
async fn run_cache_maintenance(state: Arc<ServerState>) -> anyhow::Result<()> {
    match cleanup_orphaned_cache_files(Arc::clone(&state)).await {
        Ok(report) => info!(?report, "Cache cleanup completed"),
        Err(e) => error!(error = ?e, "Cache cleanup failed"),
    }
    if let Err(e) = cleanup_old_evaluations(Arc::clone(&state)).await {
        error!(error = ?e, "Evaluation GC failed");
    } else {
        info!("Evaluation GC completed successfully");
    }
    if let Err(e) = gradient_db::gc_orphan_derivations(
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
    // The GC passes above delete `cached_path` rows whose NAR is gone without
    // touching the producer's trust flags; demote any anchor the dispatch gate
    // would trust whose output is no longer fetchable, so its dependents stop
    // failing `InputsUnavailable` and the next eval rebuilds it.
    match gradient_db::demote_unbacked_trusted_outputs(&state.worker_db, &state.nar_storage).await {
        Ok(n) if n > 0 => info!(
            reset = n,
            "Demoted trusted producers with unfetchable outputs"
        ),
        Ok(_) => {}
        Err(e) => error!(error = ?e, "Cache-trust reconcile failed"),
    }
    if let Err(e) =
        gradient_ci::unpark_storage_full_all(&state.worker_db, state.config.storage.max_storage_gb)
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
    if state.config.proto.nar_partial_ttl_secs > 0 {
        let root = format!("{}/nar-partial", state.config.storage.base_path);
        let swept = match gradient_storage::PartialStore::new(
            root,
            Duration::from_secs(state.config.proto.nar_partial_ttl_secs),
        ) {
            Ok(store) => store.gc().await,
            Err(e) => Err(e),
        };
        match swept {
            Ok(n) if n > 0 => info!(removed = n, "Stale NAR partials swept"),
            Ok(_) => {}
            Err(e) => error!(error = ?e, "NAR partial GC failed"),
        }
    }

    Ok(())
}
