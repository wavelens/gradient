/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_derivation::{Derivation, parse_drv};
use gradient_eval::ipc::{DiscoveryShard, ResolvedItem};
use gradient_eval::pool::{CallStats, EvalPool, EvalPoolConfig, LiveThunks, MemoryGuard};
use gradient_sources::{
    AttrError, DerivationResolver, EvalDependencies, FlakeDiscovery, FoundDerivations, Resolved,
    ResolvedDerivation, ShardSpec,
};
use gradient_util::store_path::nix_store_path;
use gradient_util::sync::Mutex;
use gradient_wire::types::License;
use std::sync::Arc;

use super::eval_stats::{EvalStatsAccumulator, EvalStatsTotals};
use super::exec_app::exec_spec;

#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalPool>,
    config: EvalPoolConfig,
    stats: Arc<Mutex<EvalStatsAccumulator>>,
}

struct SourceDependencies<'a>(&'a dyn EvalDependencies);

#[async_trait]
impl gradient_eval::pool::EvalDependencies for SourceDependencies<'_> {
    async fn build_imports(&self, derived_paths: Vec<String>) -> Result<(), String> {
        self.0.build_imports(derived_paths).await
    }

    async fn fetch_input(&self, request: String) -> Result<String, String> {
        self.0.fetch_input(request).await
    }
}

fn item_to_resolved(item: ResolvedItem) -> ResolvedDerivation {
    let result = match (item.drv_path, item.error) {
        (Some(drv_path), _) => item
            .exec
            .map(exec_spec)
            .transpose()
            .map(|exec| Resolved {
                drv_path,
                licenses: item.licenses.into_iter().map(wire_license).collect(),
                exec,
            })
            .map_err(|msg| anyhow::anyhow!(msg)),
        (None, Some(msg)) => Err(anyhow::anyhow!(msg)),
        (None, None) => Err(anyhow::anyhow!("eval worker returned empty result")),
    };

    (item.attr, result)
}

fn wire_license(license: gradient_eval::ipc::License) -> License {
    match license {
        gradient_eval::ipc::License::Spdx { id } => License::Spdx { id },
        gradient_eval::ipc::License::Name { name } => License::Name { name },
    }
}

fn shard_spec(shard: DiscoveryShard) -> ShardSpec {
    ShardSpec {
        pattern: shard.pattern,
        only: shard.only,
    }
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
        let config = EvalPoolConfig {
            workers: pool_size,
            max_rss,
            memory,
            cache_dir: eval_cache_dir,
            trace_dir,
            metrics: eval_metrics,
            fetch_through_parent: true,
        };
        Self {
            pool: EvalPool::new(config.clone()),
            config,
            stats: Arc::new(Mutex::new(EvalStatsAccumulator::default())),
        }
    }

    pub(crate) fn live_thunks(&self) -> Arc<LiveThunks> {
        self.pool.live_thunks()
    }

    pub fn eval_cache_dir(&self) -> &str {
        self.pool.cache_dir()
    }

    pub(crate) fn config(&self) -> &EvalPoolConfig {
        &self.config
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

    pub fn take_eval_stats(&self) -> EvalStatsTotals {
        self.pool.live_thunks().reset();
        let acc = std::mem::take(&mut *self.stats.lock());
        acc.finish()
    }

    fn observe(&self, calls: Vec<CallStats>) {
        let mut stats = self.stats.lock();
        for call in calls {
            stats.observe(call);
        }
    }
}

#[async_trait]
impl DerivationResolver for WorkerPoolResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
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
                &SourceDependencies(dependencies),
                &found_items,
            )
            .await?;
        self.observe(discovery.calls);
        Ok(FlakeDiscovery {
            warnings: discovery.warnings,
            errors: discovery.errors.into_iter().map(attr_error).collect(),
            ..Default::default()
        })
    }

    async fn plan_shards(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
    ) -> Result<(Vec<ShardSpec>, Vec<AttrError>)> {
        let (shards, errors) = self
            .pool
            .plan(
                repository,
                wildcards,
                overrides,
                &SourceDependencies(dependencies),
            )
            .await?;

        Ok((
            shards.into_iter().map(shard_spec).collect(),
            errors.into_iter().map(attr_error).collect(),
        ))
    }

    async fn list_given_shards(
        &self,
        repository: String,
        shards: Vec<ShardSpec>,
        excludes: &[String],
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
        found: FoundDerivations<'_>,
    ) -> Result<FlakeDiscovery> {
        let found_items = |items: Vec<ResolvedItem>| {
            found(items.into_iter().map(item_to_resolved).collect());
        };
        let shards = shards
            .into_iter()
            .map(|shard| DiscoveryShard {
                pattern: shard.pattern,
                only: shard.only,
            })
            .collect();
        let discovery = self
            .pool
            .list_given(
                repository,
                shards,
                excludes,
                overrides,
                &SourceDependencies(dependencies),
                &found_items,
            )
            .await?;
        self.observe(discovery.calls);
        Ok(FlakeDiscovery {
            warnings: discovery.warnings,
            errors: discovery.errors.into_iter().map(attr_error).collect(),
            deferred: discovery.deferred.into_iter().map(shard_spec).collect(),
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
