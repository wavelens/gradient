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
use gradient_db::DbContext;
use gradient_graph::Graph;
use gradient_util::supervision::{ChildSpec, Pass, PassError, RunsOn};
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

pub(crate) fn child_specs(state: &ServerState) -> Vec<ChildSpec> {
    let config = &state.config;
    let secs = |s: std::num::NonZeroU64| Duration::from_secs(s.get());
    vec![
        ChildSpec::pass(CacheMaintenance {
            db: state.db(),
            graph: Arc::clone(&state.graph),
            period: secs(config.gc.interval_secs),
        }),
        ChildSpec::pass(StorageMaintenance {
            db: state.db(),
            graph: Arc::clone(&state.graph),
            period: Duration::from_millis(config.gc.deep_pace_ms.max(1)),
        }),
        ChildSpec::pass(SignSweep {
            db: state.worker_db.clone(),
            config: Arc::clone(config),
            store_path_mapping: Arc::clone(&state.extensions.store_path_mapping),
            period: secs(config.cache.sign_sweep_interval_secs),
        }),
        ChildSpec::pass(DebugIndex {
            db: state.worker_db.clone(),
            nar_storage: state.nar_storage.clone(),
            period: secs(config.cache.debug_index_interval_secs),
        }),
        ChildSpec::pass(EvalCacheSweep {
            db: state.worker_db.clone(),
            config: Arc::clone(config),
            nar_storage: state.nar_storage.clone(),
            period: secs(config.eval.cache_sweep_interval_secs),
        }),
    ]
}

struct StorageMaintenance {
    db: DbContext,
    graph: Arc<Graph>,
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
        self.maintain().await.map_err(Into::into)
    }
}

impl StorageMaintenance {
    async fn maintain(&self) -> anyhow::Result<()> {
        loop {
            let step = match storage_migrations::step(&self.db).await? {
                Step::Idle => deep_gc::step(&self.db, &self.graph).await?,
                step => step,
            };
            if step != Step::Requested {
                return Ok(());
            }
        }
    }
}

struct CacheMaintenance {
    db: DbContext,
    graph: Arc<Graph>,
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
        self.maintain().await.map_err(Into::into)
    }
}

impl CacheMaintenance {
    async fn maintain(&self) -> anyhow::Result<()> {
        log_failure(
            "evaluation GC",
            evaluations::cleanup_old_evaluations(&self.db, &self.graph).await,
        );
        log_failure(
            "orphan derivation GC",
            evaluations::collect_orphan_derivations(&self.db, &self.graph).await,
        );
        log_failure(
            "cached-path eviction",
            nar::expiry::evict_expired_cached_paths(&self.db, &self.graph).await,
        );
        log_failure(
            "storage-full unpark",
            gradient_ci::unpark_storage_full_all(
                &self.db.worker_db,
                self.db.config.cache.max_storage_gb,
            )
            .await,
        );
        log_failure(
            "build-request blob GC",
            blobs::cleanup_unused_build_request_blobs(&self.db).await,
        );
        log_failure(
            "upload-session GC",
            blobs::cleanup_expired_upload_sessions(&self.db).await,
        );
        Ok(())
    }
}

fn log_failure<T, E: Debug>(step: &str, result: Result<T, E>) {
    if let Err(e) = result {
        error!(error = ?e, step, "cache maintenance step failed");
    }
}
