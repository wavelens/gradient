/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod dependencies;
pub mod download;
mod driver;
pub mod fan_out;
mod live_thunks;
pub mod memory;
mod transport;
mod workers;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use anyhow::Result;

use crate::ipc::{AttrError, ResolvedItem};

use self::fan_out::{
    DiscoveryCall, MAX_CRASH_ATTEMPTS, crashed_listing, discovery_calls, entry_point_of,
    pooled_fan_out,
};
use self::transport::{Listing, SpawnConfig};
use self::workers::EvalWorkerPool;

pub use self::dependencies::{EvalDependencies, RefuseDependencies};
pub use self::download::{DownloadSink, forward_nix_log};
pub use self::driver::run_eval_driver;
pub use self::fan_out::{CallStats, Discovery, FoundItems};
pub use self::live_thunks::LiveThunks;
pub use self::memory::{MemoryGuard, memory_guard_bytes, min_free_ram_guard, total_memory_bytes};
pub use self::transport::EvalWorker;
pub use self::workers::default_workers;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone)]
pub struct EvalPoolConfig {
    pub workers: usize,
    pub max_rss: u64,
    pub memory: MemoryGuard,
    pub cache_dir: String,
    pub trace_dir: Option<String>,
    pub metrics: bool,
    pub fetch_through_parent: bool,
}

#[derive(Debug)]
pub struct EvalPool {
    workers: Arc<EvalWorkerPool>,
    patterns: Mutex<Vec<String>>,
    cache_dir: String,
}

impl EvalPool {
    pub fn new(config: EvalPoolConfig) -> Arc<Self> {
        let thunks = Arc::new(LiveThunks::default());
        let workers = Arc::new(EvalWorkerPool::new(
            config.workers,
            config.max_rss,
            config.memory,
            SpawnConfig {
                cache_dir: config.cache_dir.clone(),
                trace_dir: config.trace_dir,
                metrics: config.metrics,
                fetch_through_parent: config.fetch_through_parent,
            },
            thunks,
        ));
        if tokio::runtime::Handle::try_current().is_ok() {
            let guard = config.memory;
            let weak = Arc::downgrade(&workers);
            #[expect(
                clippy::disallowed_methods,
                reason = "a Weak on the pool, ending with it"
            )]
            tokio::spawn(workers::reap_loop(weak, guard));
        }
        Arc::new(Self {
            workers,
            patterns: Mutex::new(Vec::new()),
            cache_dir: config.cache_dir,
        })
    }

    pub fn workers(&self) -> usize {
        self.workers.max()
    }

    pub fn cache_dir(&self) -> &str {
        &self.cache_dir
    }

    pub fn live_thunks(&self) -> Arc<LiveThunks> {
        self.workers.live_thunks()
    }

    pub async fn release(&self) {
        self.workers.release_idle().await;
    }

    pub async fn shutdown(&self) {
        self.workers.shutdown().await;
    }

    /// A `List` per attribute. Used when every pattern is concrete, where a discovery walk of the
    /// siblings costs orders of magnitude more than the evaluation itself.
    pub async fn list_attrs(
        &self,
        repository: String,
        attrs: Vec<String>,
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
        found: FoundItems<'_>,
    ) -> Result<Discovery> {
        let requested = attrs.clone();
        let listed = Mutex::new(HashSet::<String>::new());
        let recording = |items: Vec<ResolvedItem>| {
            lock(&listed).extend(items.iter().map(|item| item.attr.clone()));
            found(items);
        };
        let calls: Vec<DiscoveryCall> = attrs
            .into_iter()
            .map(|attr| DiscoveryCall {
                wildcards: vec![attr],
                only: None,
            })
            .collect();
        let mut discovery = self
            .run_calls(repository, calls, &[], overrides, dependencies, &recording)
            .await?;

        let listed = listed.into_inner().unwrap_or_default();
        for attr in requested {
            let reported = discovery.errors.iter().any(|error| error.attr == attr);
            if !listed.contains(&attr) && !reported {
                discovery.errors.push(AttrError {
                    message: format!("target '{attr}' matched no derivations in the flake"),
                    attr,
                });
            }
        }
        discovery.errors.sort_unstable();
        Ok(discovery)
    }

    /// Plan, then list every shard over the pool, splitting crashed batches.
    pub async fn list_shards(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
        found: FoundItems<'_>,
    ) -> Result<Discovery> {
        *lock(&self.patterns) = wildcards
            .iter()
            .filter(|w| !w.starts_with('!'))
            .cloned()
            .collect();

        let (shards, plan_errors) = {
            let mut worker = self.workers.acquire().await?;
            match worker
                .plan(
                    repository.clone(),
                    wildcards.clone(),
                    overrides.to_vec(),
                    dependencies,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    worker.mark_dead();
                    return Err(e);
                }
            }
        };

        let excludes: Vec<String> = wildcards
            .iter()
            .filter(|w| w.starts_with('!'))
            .cloned()
            .collect();
        let calls = discovery_calls(shards, &excludes, self.workers.max());
        tracing::info!(
            calls = calls.len(),
            pool = self.workers.max(),
            "discovery split into shard batches"
        );

        let mut discovery = self
            .run_calls(repository, calls, &excludes, overrides, dependencies, found)
            .await?;
        discovery.errors.extend(plan_errors);
        discovery.errors.sort_unstable();
        discovery.errors.dedup();
        Ok(discovery)
    }

    async fn run_calls(
        &self,
        repository: String,
        calls: Vec<DiscoveryCall>,
        excludes: &[String],
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
        found: FoundItems<'_>,
    ) -> Result<Discovery> {
        let listed_attrs = Mutex::new(HashSet::<String>::new());
        let warnings = Mutex::new(Vec::<String>::new());
        let errors = Mutex::new(Vec::<AttrError>::new());
        let calls_seen = Mutex::new(Vec::<CallStats>::new());
        {
            let repo = repository.as_str();
            let (listed_attrs, warnings, errors, calls_seen) =
                (&listed_attrs, &warnings, &errors, &calls_seen);
            pooled_fan_out(self.workers.max(), calls, |call| async move {
                let (listing, call_stats) =
                    self.list_shard(repo, call, overrides, dependencies).await?;
                let fresh: Vec<ResolvedItem> = {
                    let mut listed = lock(listed_attrs);
                    listing
                        .items
                        .into_iter()
                        .filter(|item| listed.insert(item.attr.clone()))
                        .collect()
                };
                found(fresh);
                lock(warnings).extend(listing.warnings);
                lock(errors).extend(listing.errors);
                if let Some(call_stats) = call_stats {
                    lock(calls_seen).push(call_stats);
                }
                Ok(discovery_calls(
                    listing.deferred,
                    excludes,
                    self.workers.max(),
                ))
            })
            .await?;
        }

        let mut warnings = warnings.into_inner().unwrap_or_default();
        warnings.sort_unstable();
        warnings.dedup();
        let mut errors = errors.into_inner().unwrap_or_default();
        errors.sort_unstable();
        errors.dedup();
        Ok(Discovery::new(
            warnings,
            errors,
            calls_seen.into_inner().unwrap_or_default(),
        ))
    }

    async fn list_shard(
        &self,
        repository: &str,
        call: DiscoveryCall,
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
    ) -> Result<(Listing, Option<CallStats>)> {
        let mut attempt = 0;
        loop {
            match self
                .list_once(repository, call.clone(), overrides, dependencies)
                .await
            {
                Ok((listing, stats)) => return Ok((listing, Some(stats))),
                Err(crash) => {
                    attempt += 1;
                    if let Some(listing) = crashed_listing(&call, attempt, &crash) {
                        return Ok((listing, None));
                    }
                    if attempt >= MAX_CRASH_ATTEMPTS {
                        return Err(crash);
                    }
                }
            }
        }
    }

    async fn list_once(
        &self,
        repository: &str,
        call: DiscoveryCall,
        overrides: &[(String, String)],
        dependencies: &dyn EvalDependencies,
    ) -> Result<(Listing, CallStats)> {
        let pattern = call.wildcards.first().cloned().unwrap_or_default();
        let bucket = entry_point_of(&pattern, &lock(&self.patterns));
        let mut worker = self.workers.acquire().await?;
        let first_call = worker.is_fresh();
        let started = Instant::now();
        match worker
            .list(
                repository.to_string(),
                call.wildcards,
                call.only,
                overrides.to_vec(),
                dependencies,
            )
            .await
        {
            Ok(mut listing) => {
                let rss = worker.rss_bytes();
                let stats = listing.stats.take();
                if let Some(delta) = stats {
                    self.workers
                        .live_thunks()
                        .commit(worker.spawned_pid(), delta.nr_thunks);
                }
                if rss > self.workers.max_rss() {
                    worker.mark_dead();
                }
                let attrs = listing
                    .items
                    .iter()
                    .filter_map(|item| item.cost.map(|cost| (item.attr.clone(), cost)))
                    .collect();
                Ok((
                    listing,
                    CallStats {
                        entry_point: bucket,
                        pattern,
                        first_call,
                        stats,
                        eval_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        attrs,
                        rss,
                    },
                ))
            }
            Err(e) => {
                self.workers.live_thunks().forget(worker.spawned_pid());
                worker.mark_dead();
                Err(e)
            }
        }
    }

    pub async fn fingerprint(
        &self,
        repository: String,
        overrides: &[(String, String)],
    ) -> Result<Option<String>> {
        let mut worker = self.workers.acquire().await?;
        match worker.fingerprint(repository, overrides.to_vec()).await {
            Ok(v) => Ok(v),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::EvalResponse;
    use crate::pool::workers::tests::{replying_worker, replying_worker_times};

    fn pool(workers: usize) -> Arc<EvalPool> {
        EvalPool::new(EvalPoolConfig {
            workers,
            max_rss: u64::MAX,
            memory: MemoryGuard::None,
            cache_dir: String::new(),
            trace_dir: None,
            metrics: false,
            fetch_through_parent: false,
        })
    }

    #[tokio::test]
    async fn concrete_attributes_are_listed_without_a_plan() {
        let pool = pool(1);
        pool.workers.push_for_test(replying_worker(
            &EvalResponse::ListOk {
                items: vec![ResolvedItem {
                    attr: "packages.x86_64-linux.hello".into(),
                    drv_path: Some("/nix/store/aaaa-hello.drv".into()),
                    licenses: vec![],
                    error: None,
                    cost: None,
                }],
                deferred: vec![],
                warnings: vec![],
                errors: vec![],
                stats: None,
            },
            "concrete-list",
        ));

        let found = Mutex::new(Vec::new());
        let found_ref = &found;
        let discovery = pool
            .list_attrs(
                "repo".into(),
                vec!["packages.x86_64-linux.hello".into()],
                &[],
                &RefuseDependencies("the test"),
                &move |items| lock(found_ref).extend(items),
            )
            .await
            .expect("list");

        assert_eq!(lock(&found).len(), 1);
        assert!(discovery.errors.is_empty());
    }

    #[tokio::test]
    async fn each_listed_attribute_reports_its_own_cost() {
        let cost = |thunks| crate::stats::AttrCost {
            stats: crate::stats::StatsDelta {
                nr_thunks: thunks,
                ..Default::default()
            },
            eval_ms: thunks / 10,
        };
        let item = |attr: &str, thunks| ResolvedItem {
            attr: attr.into(),
            drv_path: Some(format!("/nix/store/aaaa-{attr}.drv")),
            licenses: vec![],
            error: None,
            cost: Some(cost(thunks)),
        };
        let pool = pool(1);
        pool.workers.push_for_test(replying_worker(
            &EvalResponse::ListOk {
                items: vec![item("hydraJobs.a", 600), item("hydraJobs.b", 300)],
                deferred: vec![],
                warnings: vec![],
                errors: vec![],
                stats: Some(cost(1000).stats),
            },
            "costed-list",
        ));

        let discovery = pool
            .list_attrs(
                "repo".into(),
                vec!["hydraJobs.a".into()],
                &[],
                &RefuseDependencies("the test"),
                &|_| {},
            )
            .await
            .expect("list");

        let charged: Vec<(&str, u64, u64)> = discovery
            .calls
            .iter()
            .flat_map(|call| &call.attrs)
            .map(|(attr, cost)| (attr.as_str(), cost.stats.nr_thunks, cost.eval_ms))
            .collect();
        assert_eq!(
            charged,
            vec![("hydraJobs.a", 600, 60), ("hydraJobs.b", 300, 30)]
        );
    }

    #[tokio::test]
    async fn only_the_first_list_of_a_subprocess_is_a_first_call() {
        let pool = pool(1);
        pool.workers.push_for_test(replying_worker_times(
            &EvalResponse::ListOk {
                items: vec![],
                deferred: vec![],
                warnings: vec![],
                errors: vec![],
                stats: None,
            },
            "first-call",
            2,
        ));

        let discovery = pool
            .list_attrs(
                "repo".into(),
                vec!["hydraJobs.a".into(), "hydraJobs.b".into()],
                &[],
                &RefuseDependencies("the test"),
                &|_| {},
            )
            .await
            .expect("list");

        let first_calls: Vec<bool> = discovery.calls.iter().map(|call| call.first_call).collect();
        assert_eq!(first_calls, vec![true, false]);
    }

    #[tokio::test]
    async fn a_concrete_attribute_that_matches_nothing_is_reported() {
        let pool = pool(1);
        pool.workers.push_for_test(replying_worker(
            &EvalResponse::ListOk {
                items: vec![],
                deferred: vec![],
                warnings: vec![],
                errors: vec![],
                stats: None,
            },
            "empty-list",
        ));

        let discovery = pool
            .list_attrs(
                "repo".into(),
                vec!["packages.x86_64-linux.nope".into()],
                &[],
                &RefuseDependencies("the test"),
                &|_| {},
            )
            .await
            .expect("list");

        assert_eq!(discovery.errors.len(), 1, "{:?}", discovery.errors);
        assert_eq!(discovery.errors[0].attr, "packages.x86_64-linux.nope");
    }
}
