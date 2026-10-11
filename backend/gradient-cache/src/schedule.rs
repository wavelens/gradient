/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::debug_index::DebugIndex;
use crate::eval_cache::EvalCacheSweep;
use crate::signatures::SignSweep;
use crate::units::Step;
use crate::{blobs, deep_gc, evaluations, nar, storage_migrations};
use gradient_core::ServerState;
use gradient_util::supervision::{ChildSpec, Pass, PassError, RunsOn};
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

pub(crate) fn child_specs(state: &Arc<ServerState>) -> Vec<ChildSpec> {
    let config = &state.config;
    let secs = |s: std::num::NonZeroU64| Duration::from_secs(s.get());
    vec![
        ChildSpec::pass(CacheMaintenance {
            state: Arc::clone(state),
            period: secs(config.gc.interval_secs),
        }),
        ChildSpec::pass(StorageMaintenance {
            state: Arc::clone(state),
            period: Duration::from_millis(config.gc.deep_pace_ms.max(1)),
        }),
        ChildSpec::pass(SignSweep {
            state: Arc::clone(state),
            period: secs(config.cache.sign_sweep_interval_secs),
        }),
        ChildSpec::pass(DebugIndex {
            state: Arc::clone(state),
            period: secs(config.cache.debug_index_interval_secs),
        }),
        ChildSpec::pass(EvalCacheSweep {
            state: Arc::clone(state),
            period: secs(config.eval.cache_sweep_interval_secs),
        }),
    ]
}

struct StorageMaintenance {
    state: Arc<ServerState>,
    period: Duration,
}

impl Pass for StorageMaintenance {
    const NAME: &'static str = "storage-maintenance";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = Duration::from_secs(3600);

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        run_storage_maintenance(Arc::clone(&self.state))
            .await
            .map_err(Into::into)
    }
}

struct CacheMaintenance {
    state: Arc<ServerState>,
    period: Duration,
}

impl Pass for CacheMaintenance {
    const NAME: &'static str = "cache-maintenance";
    const RUNS_ON: RunsOn = RunsOn::SingleInstance;
    const BUDGET: Duration = Duration::from_secs(1800);

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        run_cache_maintenance(Arc::clone(&self.state))
            .await
            .map_err(Into::into)
    }
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
