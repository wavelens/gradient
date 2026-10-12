/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod download;
mod driver;
pub mod fan_out;
pub mod import;
mod live_thunks;
pub mod memory;
mod transport;
mod workers;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Result;

use crate::ipc::{AttrError, ResolvedItem};

use self::fan_out::{
    CallStats, Discovery, DiscoveryCall, FoundItems, MAX_CRASH_ATTEMPTS, crashed_listing,
    discovery_calls, entry_point_of, pooled_fan_out,
};
use self::transport::{EvalErrorResponse, Listing};
use self::workers::EvalWorkerPool;

pub use self::download::{DownloadSink, forward_nix_log};
pub use self::driver::run_eval_driver;
pub use self::fan_out::{CallStats as CallCost, Discovery as DiscoveryReport, FoundItems as Found};
pub use self::import::{ImportBuilder, RefuseImports};
pub use self::live_thunks::LiveThunks;
pub use self::memory::{MemoryGuard, memory_guard_bytes, min_free_ram_guard, total_memory_bytes};
pub use self::transport::EvalErrorResponse as EvalCallError;
pub use self::workers::default_workers;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct EvalPoolConfig {
    pub workers: usize,
    pub max_rss: u64,
    pub memory: MemoryGuard,
    pub cache_dir: String,
    pub trace_dir: Option<String>,
    pub metrics: bool,
}

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
            config.cache_dir.clone(),
            config.trace_dir,
            config.metrics,
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
        imports: &dyn ImportBuilder,
        found: FoundItems<'_>,
    ) -> Result<Discovery> {
        let calls: Vec<DiscoveryCall> = attrs
            .into_iter()
            .map(|attr| DiscoveryCall {
                wildcards: vec![attr],
                only: None,
            })
            .collect();
        self.run_calls(repository, calls, &[], overrides, imports, found)
            .await
    }

    /// Plan, then list every shard over the pool, splitting crashed batches.
    pub async fn list_shards(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
        imports: &dyn ImportBuilder,
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
                    imports,
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
            .run_calls(repository, calls, &excludes, overrides, imports, found)
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
        imports: &dyn ImportBuilder,
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
                let (listing, call_stats) = self.list_shard(repo, call, overrides, imports).await?;
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
        imports: &dyn ImportBuilder,
    ) -> Result<(Listing, Option<CallStats>)> {
        let mut attempt = 0;
        loop {
            match self
                .list_once(repository, call.clone(), overrides, imports)
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
        imports: &dyn ImportBuilder,
    ) -> Result<(Listing, CallStats)> {
        let bucket = entry_point_of(
            call.wildcards
                .first()
                .map(String::as_str)
                .unwrap_or_default(),
            &lock(&self.patterns),
        );
        let mut worker = self.workers.acquire().await?;
        match worker
            .list(
                repository.to_string(),
                call.wildcards,
                call.only,
                overrides.to_vec(),
                imports,
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
                Ok((
                    listing,
                    CallStats {
                        entry_point: bucket,
                        stats,
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

    pub async fn checkpoint(
        &self,
        repository: String,
        overrides: &[(String, String)],
    ) -> Result<()> {
        let mut worker = self.workers.acquire().await?;
        match worker.checkpoint(repository, overrides.to_vec()).await {
            Ok(()) => Ok(()),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    pub async fn fetch_input(
        &self,
        locked: String,
        git_ssh_command: Option<String>,
        sink: Arc<dyn DownloadSink>,
    ) -> Result<String> {
        let mut worker = self.workers.acquire().await?;
        let fetched = worker.fetch_input(locked, git_ssh_command, sink).await;
        if fetched
            .as_ref()
            .is_err_and(|e| !e.is::<EvalErrorResponse>())
        {
            worker.mark_dead();
        }
        fetched
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::EvalResponse;
    use crate::pool::workers::tests::replying_worker;

    fn pool(workers: usize) -> Arc<EvalPool> {
        EvalPool::new(EvalPoolConfig {
            workers,
            max_rss: u64::MAX,
            memory: MemoryGuard::None,
            cache_dir: String::new(),
            trace_dir: None,
            metrics: false,
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
                    references: vec![],
                    error: None,
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
                &RefuseImports("the test"),
                &move |items| lock(found_ref).extend(items),
            )
            .await
            .expect("list");

        assert_eq!(lock(&found).len(), 1);
        assert!(discovery.errors.is_empty());
    }

    struct NullSink;

    impl DownloadSink for NullSink {
        fn transfer(&self, _id: u64, _done: u64, _expected: u64) {}
    }

    #[tokio::test]
    async fn a_failed_fetch_keeps_the_worker_unless_its_pipe_broke() {
        let pool = pool(1);
        pool.workers.push_for_test(replying_worker(
            &EvalResponse::Err {
                message: "404".into(),
            },
            "fetch-404",
        ));

        let err = pool
            .fetch_input("{}".into(), None, Arc::new(NullSink))
            .await
            .expect_err("a refused fetch is an error");

        assert_eq!(err.to_string(), "eval worker: 404");
        assert_eq!(pool.workers.idle_count(), 1, "an answered error keeps it");
    }
}
