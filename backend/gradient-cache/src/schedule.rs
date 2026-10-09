/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::units::Step;
use crate::{
    blobs, debug_index, deep_gc, eval_cache, evaluations, nar, signatures, storage_migrations,
};
use futures::future::BoxFuture;
use gradient_core::ServerState;
use gradient_util::supervision::ChildSpec;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

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
            signatures::sign_missing_signatures,
        ),
        Sweep::new(
            "debug-index",
            secs(config.cache.debug_index_interval_secs),
            600,
            debug_index::index_pending_debug_info,
        ),
        Sweep::new(
            "eval-cache-sweep",
            secs(config.eval.cache_sweep_interval_secs),
            600,
            eval_cache::evict_eval_cache,
        ),
    ]
}

pub(crate) fn child_specs(state: &Arc<ServerState>) -> Vec<ChildSpec> {
    sweeps(state)
        .into_iter()
        .map(|sweep| {
            let state = Arc::clone(state);
            let run = sweep.run;
            ChildSpec::periodic(sweep.name, sweep.interval, sweep.budget, move || {
                let fut = run(Arc::clone(&state));
                async move { fut.await.map_err(Into::into) }
            })
        })
        .collect()
}

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

async fn run_cache_maintenance(state: Arc<ServerState>) -> anyhow::Result<()> {
    log_failure(
        "evaluation GC",
        evaluations::cleanup_old_evaluations(Arc::clone(&state)).await,
    );
    log_failure(
        "orphan derivation GC",
        evaluations::collect_orphan_derivations(&state).await,
    );
    log_failure(
        "cached-path eviction",
        nar::expiry::evict_expired_cached_paths(Arc::clone(&state)).await,
    );
    log_failure(
        "storage-full unpark",
        gradient_ci::unpark_storage_full_all(&state.worker_db, state.config.cache.max_storage_gb)
            .await,
    );
    log_failure(
        "build-request blob GC",
        blobs::cleanup_unused_build_request_blobs(Arc::clone(&state)).await,
    );
    log_failure(
        "upload-session GC",
        blobs::cleanup_expired_upload_sessions(Arc::clone(&state)).await,
    );
    Ok(())
}

fn log_failure<T, E: Debug>(step: &str, result: Result<T, E>) {
    if let Err(e) = result {
        error!(error = ?e, step, "cache maintenance step failed");
    }
}
