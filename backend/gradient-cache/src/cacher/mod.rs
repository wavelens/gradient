/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-side cache maintenance.
//!
//! Workers pack and compress NARs; the server signs them on upload. This
//! module runs the periodic sweeps: cleanup, signature and debug-index
//! backfills, eval cache eviction, and the storage migrations and deep GC.

mod cleanup;
mod debug_index;
mod deep_gc;
mod eval_cache_sweep;
mod invalidate;
mod sign_sweep;
mod storage_migrations;
#[cfg(test)]
pub(crate) mod test_support;
mod units;

pub use self::debug_index::index_pending_debug_info;
pub use self::deep_gc::DeepGcReport;
pub use self::eval_cache_sweep::evict_eval_cache;

pub use self::cleanup::{
    CleanupReport, cleanup_expired_upload_sessions, cleanup_old_evaluations,
    cleanup_stale_build_request_blobs, evict_stale_cached_paths, reconcile_nar_shard,
};
pub use self::invalidate::invalidate_cache_for_path;
pub use self::sign_sweep::sign_missing_signatures;

use futures::future::BoxFuture;
use gradient_core::ServerState;
use gradient_util::supervision::ChildSpec;
use std::sync::Arc;
use self::units::Step;
use std::time::Duration;
use tracing::{error, info, warn};

/// One periodic pass: a name (logs and health), a tick interval, the budget
/// past which a pass is cancelled, and the async fn to run.
struct Sweep {
    name: &'static str,
    interval: Duration,
    budget: Duration,
    run: Box<dyn Fn(Arc<ServerState>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
}

impl Sweep {
    fn new<F, Fut>(name: &'static str, interval: Duration, budget_secs: u64, run: F) -> Self
    where
        F: Fn(Arc<ServerState>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Sweep {
            name,
            interval,
            budget: Duration::from_secs(budget_secs),
            run: Box::new(move |state| Box::pin(run(state))),
        }
    }
}

/// The registered sweeps. "cache-maintenance" bundles the order-sensitive
/// GC/reconcile steps; "storage-maintenance" the storage migrations and the
/// deep GC; "sign-sweep" is the signature backfill, "debug-index" the build-id
/// backfill, "eval-cache-sweep" the eval-cache eviction.
fn sweeps(state: &ServerState) -> Vec<Sweep> {
    let config = &state.config;
    let secs = |s: u64| Duration::from_secs(s.max(1));
    vec![
        Sweep::new(
            "cache-maintenance",
            secs(config.gc.interval_secs),
            1800,
            run_cache_maintenance,
        ),
        Sweep::new(
            "storage-maintenance",
            Duration::from_millis(config.gc.deep_pace_ms.max(1)),
            3600,
            run_storage_maintenance,
        ),
        Sweep::new(
            "sign-sweep",
            secs(config.cache.sign_sweep_interval_secs),
            300,
            sign_missing_signatures,
        ),
        Sweep::new(
            "debug-index",
            secs(config.cache.debug_index_interval_secs),
            600,
            index_pending_debug_info,
        ),
        Sweep::new(
            "eval-cache-sweep",
            secs(config.eval.cache_sweep_interval_secs),
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
                sweep.interval,
                sweep.budget,
                move || {
                    let fut = run(Arc::clone(&state));
                    async move { fut.await.map_err(Into::into) }
                },
            )
        })
        .collect()
}

/// The orphan-derivation pass: scan the whole keep-set once on the pool, then
/// hand the actor bounded chunks to apply. The log files of what it actually
/// deleted are reclaimed here, because they live outside the database.
async fn run_derivation_gc(state: &Arc<ServerState>) -> anyhow::Result<usize> {
    let (candidates, scanned_at) = gradient_db::orphan_derivation_candidates(
        &state.worker_db,
        state.config.gc.orphan_derivation_hours,
    )
    .await?;

    let mut deleted = 0usize;
    for chunk in candidates.chunks(gradient_db::IN_CHUNK_SIZE) {
        let report = match state
            .graph
            .gc(gradient_graph::GcRequest::Derivations {
                candidates: chunk.to_vec(),
                scanned_at,
            })
            .await
        {
            Ok(report) => report,
            Err(e) => {
                warn!(error = %e, "GC: orphan derivation delete chunk failed; skipping");
                continue;
            }
        };

        deleted += report.deleted_derivations.len();
        for attempt in report.attempt_logs {
            if let Err(e) = state.log_storage.delete(attempt).await {
                warn!(error = %e, %attempt, "GC: failed to remove orphan build log");
            }
        }
    }

    Ok(deleted)
}

/// One tick of the background storage work: a pending storage migration
/// first, then the deep GC. A requested deep GC round runs unit after unit
/// within the tick; anything else runs one unit and waits for the pace.
async fn run_storage_maintenance(state: Arc<ServerState>) -> anyhow::Result<()> {
    loop {
        let step = match storage_migrations::step(&state).await? {
            Step::Idle => deep_gc::step(&state).await?,
            step => step,
        };
        if step != Step::Requested {
            return Ok(());
        }
    }
}

/// The order-sensitive cache-maintenance steps, run sequentially every
/// `gc.interval_secs`. No per-output work here - the worker uploads+signs; this
/// is GC and self-heal reconciliation only. Storage objects are reconciled by
/// the deep GC.
async fn run_cache_maintenance(state: Arc<ServerState>) -> anyhow::Result<()> {
    if let Err(e) = cleanup_old_evaluations(Arc::clone(&state)).await {
        error!(error = ?e, "Evaluation GC failed");
    } else {
        info!("Evaluation GC completed successfully");
    }
    match run_derivation_gc(&state).await {
        Ok(deleted) => info!(deleted, "Derivation GC completed successfully"),
        Err(e) => error!(error = ?e, "Derivation GC failed"),
    }
    match evict_stale_cached_paths(Arc::clone(&state)).await {
        Ok(n) if n > 0 => info!(evicted = n, "Stale cached-path eviction completed"),
        Ok(_) => {}
        Err(e) => error!(error = ?e, "Stale cached-path eviction failed"),
    }
    if let Err(e) =
        gradient_ci::unpark_storage_full_all(&state.worker_db, state.config.cache.max_storage_gb)
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
    Ok(())
}
