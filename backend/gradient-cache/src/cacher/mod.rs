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
mod debug_index;
mod deep_gc;
mod eval_cache_sweep;
mod invalidate;
mod sign_sweep;
#[cfg(test)]
pub(crate) mod test_support;

pub use self::debug_index::index_pending_debug_info;
pub use self::deep_gc::{DeepGcReport, run_deep_gc};
pub use self::eval_cache_sweep::evict_eval_cache;

pub use self::cleanup::{
    CleanupReport, cleanup_expired_upload_sessions, cleanup_old_evaluations,
    cleanup_orphaned_cache_files, cleanup_stale_build_request_blobs, cleanup_stale_cached_nars,
};
pub use self::invalidate::invalidate_cache_for_path;
pub use self::sign_sweep::sign_missing_signatures;

use futures::future::BoxFuture;
use gradient_core::ServerState;
use gradient_util::supervision::ChildSpec;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

/// One periodic pass: a name (logs and health), a tick interval, the budget
/// past which a pass is cancelled, and the async fn to run.
struct Sweep {
    name: &'static str,
    interval_secs: u64,
    budget_secs: u64,
    run: Box<dyn Fn(Arc<ServerState>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
}

impl Sweep {
    fn new<F, Fut>(name: &'static str, interval_secs: u64, budget_secs: u64, run: F) -> Self
    where
        F: Fn(Arc<ServerState>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Sweep {
            name,
            interval_secs,
            budget_secs,
            run: Box::new(move |state| Box::pin(run(state))),
        }
    }
}

/// The registered sweeps. "cache-maintenance" bundles the 9 order-sensitive
/// GC/reconcile steps; "sign-sweep" is the signature backfill, "debug-index"
/// the build-id backfill, "eval-cache-sweep" the eval-cache eviction.
fn sweeps(state: &ServerState) -> Vec<Sweep> {
    let storage = &state.config.storage;
    vec![
        Sweep::new(
            "cache-maintenance",
            storage.cache_maintenance_interval_secs.max(1),
            1800,
            run_cache_maintenance,
        ),
        Sweep::new(
            "sign-sweep",
            storage.sign_sweep_interval_secs.max(1),
            300,
            sign_missing_signatures,
        ),
        Sweep::new(
            "debug-index",
            storage.debug_index_interval_secs.max(1),
            600,
            index_pending_debug_info,
        ),
        Sweep::new(
            "eval-cache-sweep",
            storage.eval_cache_sweep_interval_secs.max(1),
            600,
            evict_eval_cache,
        ),
    ]
}

/// Every registered sweep as a supervised periodic child.
pub fn child_specs(state: &Arc<ServerState>) -> Vec<ChildSpec> {
    sweeps(state)
        .into_iter()
        .map(|sweep| {
            let state = Arc::clone(state);
            let run = sweep.run;
            ChildSpec::periodic(
                sweep.name,
                Duration::from_secs(sweep.interval_secs),
                Duration::from_secs(sweep.budget_secs),
                move || {
                    let fut = run(Arc::clone(&state));
                    async move { fut.await.map_err(Into::into) }
                },
            )
        })
        .collect()
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
