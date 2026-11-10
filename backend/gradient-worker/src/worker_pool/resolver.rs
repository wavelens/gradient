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
    AttrError, DerivationResolver, EvalDependencies, FlakeDiscovery, FlakeEvalTarget,
    FoundDerivations, Resolved, ResolvedDerivation, ShardSpec,
};
use gradient_util::store_path::nix_store_path;
use gradient_util::sync::Mutex;
use gradient_wire::types::License;
use gradient_wire::types::NixEvaluationFetchUrlAccess;
use std::sync::Arc;

use super::eval_stats::{EvalStatsAccumulator, EvalStatsTotals};
use super::exec_app::exec_spec;

#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalPool>,
    config: EvalPoolConfig,
    stats: Mutex<EvalStatsAccumulator>,
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

fn eval_target_to_flake_eval_target(
    eval_target: &FlakeEvalTarget,
) -> gradient_eval::ipc::FlakeEvalTarget {
    use gradient_eval::ipc::NixEvaluationFetchUrlAccess as IpcAccess;

    gradient_eval::ipc::FlakeEvalTarget {
        repository: eval_target.repository.clone(),
        input_overrides: eval_target.input_overrides.clone(),
        nix_evaluation_fetch_url_access: match &eval_target.nix_evaluation_fetch_url_access {
            NixEvaluationFetchUrlAccess::UnrestrictedEvaluation => {
                IpcAccess::UnrestrictedEvaluation
            }
            NixEvaluationFetchUrlAccess::LockedInputsPlusExtraUrlPrefixes {
                extra_allowed_uri_prefixes,
            } => IpcAccess::LockedInputsPlusExtraUrlPrefixes {
                extra_allowed_uri_prefixes: extra_allowed_uri_prefixes.clone(),
            },
        },
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
            stats: Mutex::default(),
        }
    }

    pub(crate) fn evaluation_resolver_with_own_statistics(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            config: self.config.clone(),
            stats: Mutex::default(),
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

    pub async fn fingerprint(&self, eval_target: &FlakeEvalTarget) -> Result<Option<String>> {
        self.pool
            .fingerprint(&eval_target_to_flake_eval_target(eval_target))
            .await
    }

    pub fn take_eval_stats(&self) -> EvalStatsTotals {
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
        eval_target: &FlakeEvalTarget,
        wildcards: Vec<String>,
        run_overrides: Option<&str>,
        dependencies: &dyn EvalDependencies,
        found: FoundDerivations<'_>,
    ) -> Result<FlakeDiscovery> {
        let found_items = |items: Vec<ResolvedItem>| {
            found(items.into_iter().map(item_to_resolved).collect());
        };
        let discovery = self
            .pool
            .list_shards(
                &eval_target_to_flake_eval_target(eval_target),
                wildcards,
                run_overrides,
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
        eval_target: &FlakeEvalTarget,
        wildcards: Vec<String>,
        dependencies: &dyn EvalDependencies,
    ) -> Result<(Vec<ShardSpec>, Vec<AttrError>)> {
        let (shards, errors) = self
            .pool
            .plan(
                &eval_target_to_flake_eval_target(eval_target),
                wildcards,
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
        eval_target: &FlakeEvalTarget,
        shards: Vec<ShardSpec>,
        wildcards: &[String],
        run_overrides: Option<&str>,
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
                &eval_target_to_flake_eval_target(eval_target),
                shards,
                wildcards,
                run_overrides,
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_eval::stats::StatsDelta;

    fn call(thunks: u64) -> CallStats {
        CallStats {
            entry_point: "packages.x86_64-linux.a".into(),
            pattern: "packages.x86_64-linux.a".into(),
            first_call: false,
            stats: Some(StatsDelta {
                nr_thunks: thunks,
                ..Default::default()
            }),
            eval_ms: 0,
            attrs: vec![],
            rss: 0,
        }
    }

    #[tokio::test]
    async fn each_evaluation_takes_only_its_own_statistics() {
        let shared = WorkerPoolResolver::new(
            1,
            0,
            MemoryGuard::MaxTotal(1024 * 1024 * 1024),
            String::new(),
            None,
            false,
        );
        let first = shared.evaluation_resolver_with_own_statistics();
        let second = shared.evaluation_resolver_with_own_statistics();

        first.observe(vec![call(3)]);
        second.observe(vec![call(40)]);

        assert_eq!(first.take_eval_stats().total_thunks, 3);
        assert_eq!(second.take_eval_stats().total_thunks, 40);
    }
}
