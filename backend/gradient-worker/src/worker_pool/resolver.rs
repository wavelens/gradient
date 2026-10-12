/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_derivation::{Derivation, parse_drv};
use gradient_eval::ipc::ResolvedItem;
use gradient_eval::pool::{CallStats, EvalPool, EvalPoolConfig, LiveThunks, MemoryGuard};
use gradient_sources::{
    AttrError, DerivationResolver, FlakeDiscovery, FoundDerivations, ImportBuilder,
    ResolvedDerivation,
};
use gradient_util::store_path::nix_store_path;
use gradient_util::sync::Mutex;
use std::sync::Arc;

use super::eval_stats::{EvalStatsAccumulator, EvalStatsTotals};
use super::input_fetch::{DownloadTarget, InputFetcher};

#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalPool>,
    stats: Arc<Mutex<EvalStatsAccumulator>>,
}

struct SourceImports<'a>(&'a dyn ImportBuilder);

#[async_trait]
impl gradient_eval::pool::ImportBuilder for SourceImports<'_> {
    async fn build_imports(&self, derived_paths: Vec<String>) -> Result<(), String> {
        self.0.build_imports(derived_paths).await
    }
}

fn item_to_resolved(item: ResolvedItem) -> ResolvedDerivation {
    let result = match (item.drv_path, item.error) {
        (Some(drv), _) => Ok((drv, item.references)),
        (None, Some(msg)) => Err(anyhow::anyhow!(msg)),
        (None, None) => Err(anyhow::anyhow!("eval worker returned empty result")),
    };

    (item.attr, result)
}

fn attr_error(error: gradient_eval::ipc::AttrError) -> AttrError {
    AttrError {
        attr: error.attr,
        message: error.message,
    }
}

impl WorkerPoolResolver {
    pub fn new(
        pool_size: usize,
        max_rss: u64,
        memory: MemoryGuard,
        eval_cache_dir: String,
        trace_dir: Option<String>,
        eval_metrics: bool,
    ) -> Self {
        Self {
            pool: EvalPool::new(EvalPoolConfig {
                workers: pool_size,
                max_rss,
                memory,
                cache_dir: eval_cache_dir,
                trace_dir,
                metrics: eval_metrics,
            }),
            stats: Arc::new(Mutex::new(EvalStatsAccumulator::default())),
        }
    }

    pub(crate) fn live_thunks(&self) -> Arc<LiveThunks> {
        self.pool.live_thunks()
    }

    pub fn eval_cache_dir(&self) -> &str {
        self.pool.cache_dir()
    }

    pub async fn shutdown(&self) {
        self.pool.shutdown().await;
    }

    pub async fn fingerprint(
        &self,
        repository: String,
        overrides: &[(String, String)],
    ) -> Result<Option<String>> {
        self.pool.fingerprint(repository, overrides).await
    }

    pub async fn checkpoint_cache(
        &self,
        repository: String,
        overrides: &[(String, String)],
    ) -> Result<()> {
        self.pool.checkpoint(repository, overrides).await
    }

    pub fn take_eval_stats(&self) -> EvalStatsTotals {
        self.pool.live_thunks().reset();
        let acc = std::mem::take(&mut *self.stats.lock());
        acc.finish()
    }

    fn observe(&self, calls: Vec<CallStats>) {
        let mut stats = self.stats.lock();
        for call in calls {
            if let Some(delta) = call.stats {
                stats.observe(&call.entry_point, delta, call.rss);
            }
        }
    }
}

#[async_trait]
impl InputFetcher for WorkerPoolResolver {
    async fn fetch_input(
        &self,
        locked: String,
        git_ssh_command: Option<String>,
        target: DownloadTarget,
    ) -> Result<String> {
        self.pool
            .fetch_input(locked, git_ssh_command, Arc::new(target))
            .await
    }
}

#[async_trait]
impl DerivationResolver for WorkerPoolResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
        imports: &dyn ImportBuilder,
        found: FoundDerivations<'_>,
    ) -> Result<FlakeDiscovery> {
        let found_items = |items: Vec<ResolvedItem>| {
            found(items.into_iter().map(item_to_resolved).collect());
        };
        let discovery = self
            .pool
            .list_shards(
                repository,
                wildcards,
                overrides,
                &SourceImports(imports),
                &found_items,
            )
            .await?;
        self.observe(discovery.calls);
        Ok(FlakeDiscovery {
            warnings: discovery.warnings,
            errors: discovery.errors.into_iter().map(attr_error).collect(),
        })
    }

    async fn release_evaluators(&self) {
        self.pool.release().await;
    }

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation> {
        let full_path = nix_store_path(&drv_path);
        let bytes = tokio::fs::read(&full_path)
            .await
            .with_context(|| format!("read derivation file: {}", full_path))?;
        parse_drv(&bytes).with_context(|| format!("parse derivation {}", drv_path))
    }

    async fn get_features(&self, drv_path: String) -> Result<(String, Vec<String>)> {
        if !drv_path.ends_with(".drv") {
            return Ok((gradient_types::BUILTIN_ARCH.to_string(), vec![]));
        }
        let drv = self.get_derivation(drv_path).await?;
        let features = drv.required_system_features();
        Ok((drv.system.clone(), features))
    }
}
