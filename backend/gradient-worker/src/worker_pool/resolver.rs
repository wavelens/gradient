/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_derivation::{Derivation, parse_drv};
use gradient_eval::ipc::{DiscoveryShard, ResolvedItem};
use gradient_sources::{AttrError, DerivationResolver, FlakeDiscovery, ResolvedDerivation};
use gradient_util::store_path::nix_store_path;
use gradient_util::sync::Mutex;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;

use super::eval_stats::{EvalStatsAccumulator, EvalStatsTotals, StatsDelta};
use super::input_fetch::{DownloadTarget, InputFetcher};
use super::live_thunks::LiveThunks;
use super::pool::{EvalWorkerPool, PooledEvalWorker};
use super::transport::{EvalErrorResponse, Listing};

#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalWorkerPool>,
    eval_cache_dir: String,
    stats: Arc<Mutex<EvalStatsAccumulator>>,
    patterns: Arc<Mutex<Vec<String>>>,
    thunks: Arc<LiveThunks>,
}

const MAX_CRASH_ATTEMPTS: u32 = 2;

const MAX_BATCH: usize = 64;

fn batch_size(items: usize, workers: usize) -> usize {
    items.div_ceil(workers.max(1) * 4).clamp(1, MAX_BATCH)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveryCall {
    wildcards: Vec<String>,
    only: Option<Vec<String>>,
}

fn discovery_calls(
    shards: Vec<DiscoveryShard>,
    excludes: &[String],
    workers: usize,
) -> Vec<DiscoveryCall> {
    let wildcards = |pattern: String| {
        let mut w = Vec::with_capacity(1 + excludes.len());
        w.push(pattern);
        w.extend_from_slice(excludes);
        w
    };

    shards
        .into_iter()
        .flat_map(|shard| match shard.only {
            None => vec![DiscoveryCall {
                wildcards: wildcards(shard.pattern),
                only: None,
            }],
            Some(names) => names
                .chunks(batch_size(names.len(), workers))
                .map(|chunk| DiscoveryCall {
                    wildcards: wildcards(shard.pattern.clone()),
                    only: Some(chunk.to_vec()),
                })
                .collect(),
        })
        .collect()
}

fn entry_point_of(attr: &str, patterns: &[String]) -> String {
    let attr_segs: Vec<&str> = attr.split('.').collect();
    let matches = |pat: &str| {
        let pat_segs: Vec<&str> = pat.split('.').collect();
        pat_segs.len() <= attr_segs.len()
            && pat_segs
                .iter()
                .zip(&attr_segs)
                .all(|(p, a)| *p == "*" || p == a)
    };

    patterns
        .iter()
        .filter(|p| matches(p))
        .max_by_key(|p| {
            let segs = p.split('.').count();
            let literals = p.split('.').filter(|s| *s != "*").count();
            (segs, literals)
        })
        .cloned()
        .unwrap_or_else(|| attr_segs.first().copied().unwrap_or("").to_string())
}

/// A crashed batch of members is deferred as a shard again, which comes back in smaller batches,
/// down to the member that sinks the evaluator. A lone member is retried once and then recorded as failed.
fn crashed_listing(call: &DiscoveryCall, attempt: u32, crash: &anyhow::Error) -> Option<Listing> {
    let names = call.only.as_deref()?;
    let pattern = call.wildcards.first()?;
    let mut listing = Listing::default();
    match names {
        [] => return None,
        [_] if attempt < MAX_CRASH_ATTEMPTS => return None,
        [name] => listing.errors.push(gradient_eval::ipc::AttrError {
            attr: member_attr(pattern, name),
            message: format!("evaluator crashed while listing this attribute: {crash:#}"),
        }),
        _ => listing.deferred.push(DiscoveryShard {
            pattern: pattern.clone(),
            only: Some(names.to_vec()),
        }),
    }
    Some(listing)
}

fn member_attr(pattern: &str, name: &str) -> String {
    match pattern.rsplit_once('.') {
        Some((prefix, _)) => format!("{prefix}.{name}"),
        None => name.to_string(),
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

async fn pooled_fan_out<T, Fut>(workers: usize, items: Vec<T>, run: impl Fn(T) -> Fut) -> Result<()>
where
    Fut: Future<Output = Result<Vec<T>>>,
{
    let mut queue = VecDeque::from(items);
    let mut running = FuturesUnordered::new();
    loop {
        while running.len() < workers.max(1)
            && let Some(item) = queue.pop_front()
        {
            running.push(run(item));
        }

        let Some(done) = running.next().await else {
            return Ok(());
        };
        queue.extend(done?);
    }
}

impl WorkerPoolResolver {
    pub fn new(pool_size: usize, max_eval_rss: u64, eval_cache_dir: String) -> Self {
        let thunks = Arc::new(LiveThunks::default());
        Self {
            pool: Arc::new(EvalWorkerPool::new(
                pool_size,
                max_eval_rss,
                eval_cache_dir.clone(),
                Arc::clone(&thunks),
            )),
            eval_cache_dir,
            stats: Arc::new(Mutex::new(EvalStatsAccumulator::default())),
            patterns: Arc::new(Mutex::new(Vec::new())),
            thunks,
        }
    }

    pub(crate) fn live_thunks(&self) -> Arc<LiveThunks> {
        Arc::clone(&self.thunks)
    }

    pub fn start_memory_reaper(&self, min_free_bytes: u64) {
        self.pool.configure_memory_guard(min_free_bytes);
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }

        let weak = Arc::downgrade(&self.pool);
        #[expect(
            clippy::disallowed_methods,
            reason = "holds only a Weak on the pool and ends with it"
        )]
        tokio::spawn(super::memory::memory_reaper_loop(weak, min_free_bytes));
    }

    fn observe_stats(&self, entry_point: &str, delta: StatsDelta, rss: u64) {
        self.stats.lock().observe(entry_point, delta, rss);
    }

    fn finish_call(&self, worker: &mut PooledEvalWorker, bucket: &str, stats: Option<StatsDelta>) {
        let rss = worker.rss_bytes();
        if let Some(delta) = stats {
            self.thunks.commit(worker.spawned_pid(), delta.nr_thunks);
            self.observe_stats(bucket, delta, rss);
        }
        if rss > self.pool.max_eval_rss() {
            worker.mark_dead();
        }
    }

    fn bucket_of(&self, first_attr: Option<&String>) -> String {
        first_attr
            .map(|a| entry_point_of(a, &self.patterns.lock()))
            .unwrap_or_default()
    }

    pub fn take_eval_stats(&self) -> EvalStatsTotals {
        self.thunks.reset();
        let acc = std::mem::take(&mut *self.stats.lock());
        acc.finish()
    }

    pub fn eval_cache_dir(&self) -> &str {
        &self.eval_cache_dir
    }

    pub async fn shutdown(&self) {
        self.pool.shutdown().await;
    }

    pub async fn fingerprint(
        &self,
        repository: String,
        overrides: &[(String, String)],
    ) -> Result<Option<String>> {
        let mut worker = self.pool.acquire().await?;
        match worker.fingerprint(repository, overrides.to_vec()).await {
            Ok(v) => Ok(v),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    pub async fn checkpoint_cache(
        &self,
        repository: String,
        overrides: &[(String, String)],
    ) -> Result<()> {
        let mut worker = self.pool.acquire().await?;
        match worker.checkpoint(repository, overrides.to_vec()).await {
            Ok(()) => Ok(()),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    async fn list_shard(
        &self,
        repository: &str,
        call: DiscoveryCall,
        overrides: &[(String, String)],
    ) -> Result<Listing> {
        let mut attempt = 0;
        loop {
            match self.list_once(repository, call.clone(), overrides).await {
                Ok(v) => return Ok(v),
                Err(crash) => {
                    attempt += 1;
                    if let Some(listing) = crashed_listing(&call, attempt, &crash) {
                        return Ok(listing);
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
    ) -> Result<Listing> {
        let bucket = self.bucket_of(call.wildcards.first());
        let mut worker = self.pool.acquire().await?;
        match worker
            .list(
                repository.to_string(),
                call.wildcards,
                call.only,
                overrides.to_vec(),
            )
            .await
        {
            Ok(mut listing) => {
                self.finish_call(&mut worker, &bucket, listing.stats.take());
                Ok(listing)
            }
            Err(e) => {
                self.thunks.forget(worker.spawned_pid());
                worker.mark_dead();
                Err(e)
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
        let mut worker = self.pool.acquire().await?;
        let fetched = worker.fetch_input(locked, git_ssh_command, target).await;
        if fetched
            .as_ref()
            .is_err_and(|e| !e.is::<EvalErrorResponse>())
        {
            worker.mark_dead();
        }
        fetched
    }
}

#[async_trait]
impl DerivationResolver for WorkerPoolResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
    ) -> Result<FlakeDiscovery> {
        *self.patterns.lock() = wildcards
            .iter()
            .filter(|w| !w.starts_with('!'))
            .cloned()
            .collect();

        // Planning is forcing only the prefix attrsets.
        // A trailing wildcard is coming back as unforced child names.
        // A flat attrset of heavy children is then listed in batches across the pool.
        let (shards, plan_errors) = {
            let mut worker = self.pool.acquire().await?;
            match worker
                .plan(repository.clone(), wildcards.clone(), overrides.to_vec())
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    self.thunks.forget(worker.spawned_pid());
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
        let calls = discovery_calls(shards, &excludes, self.pool.max());
        tracing::info!(
            calls = calls.len(),
            pool = self.pool.max(),
            "discovery split into shard batches"
        );

        let items = Mutex::new(Vec::<ResolvedItem>::new());
        let warnings = Mutex::new(Vec::<String>::new());
        let errors = Mutex::new(plan_errors);
        {
            let repo = repository.as_str();
            let (items, warnings, errors) = (&items, &warnings, &errors);
            let excludes = excludes.as_slice();
            pooled_fan_out(self.pool.max(), calls, |call| async move {
                let listing = self.list_shard(repo, call, overrides).await?;
                items.lock().extend(listing.items);
                warnings.lock().extend(listing.warnings);
                errors.lock().extend(listing.errors);
                Ok(discovery_calls(listing.deferred, excludes, self.pool.max()))
            })
            .await?;
        }

        let mut items = items.into_inner();
        items.sort_by(|a, b| a.attr.cmp(&b.attr));
        items.dedup_by(|a, b| a.attr == b.attr);
        let derivations = items.into_iter().map(item_to_resolved).collect();
        let mut warnings = warnings.into_inner();
        warnings.sort_unstable();
        warnings.dedup();
        let mut errors: Vec<AttrError> = errors
            .into_inner()
            .into_iter()
            .map(|e| AttrError {
                attr: e.attr,
                message: e.message,
            })
            .collect();
        errors.sort_unstable();
        errors.dedup();

        Ok(FlakeDiscovery {
            derivations,
            warnings,
            errors,
        })
    }

    async fn release_evaluators(&self) {
        self.pool.release_idle().await;
    }

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation> {
        let full_path = nix_store_path(&drv_path);
        let bytes = tokio::fs::read(&full_path)
            .await
            .with_context(|| format!("Failed to read derivation file: {}", full_path))?;
        parse_drv(&bytes).with_context(|| format!("Failed to parse derivation {}", drv_path))
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
    use gradient_util::sync::Mutex;

    #[test]
    fn a_restricted_shard_is_listed_in_batches_carrying_the_exclusions() {
        let names: Vec<String> = (0..10).map(|i| format!("host{i}")).collect();
        let shards = vec![
            DiscoveryShard {
                pattern: "hydraJobs.*".into(),
                only: Some(names.clone()),
            },
            DiscoveryShard {
                pattern: "packages.x86_64-linux.hello".into(),
                only: None,
            },
        ];
        let excludes = vec!["!hydraJobs.host3".to_string()];

        let calls = discovery_calls(shards, &excludes, 1);

        let batched: Vec<&DiscoveryCall> = calls.iter().filter(|c| c.only.is_some()).collect();
        assert_eq!(batched.len(), 4, "10 names over 1 worker: batches of 3");
        let listed: Vec<String> = batched
            .iter()
            .flat_map(|c| c.only.clone().unwrap_or_default())
            .collect();
        assert_eq!(listed, names);
        assert!(calls.iter().all(|c| c.wildcards.ends_with(&excludes)));
        assert!(calls.contains(&DiscoveryCall {
            wildcards: vec![
                "packages.x86_64-linux.hello".into(),
                "!hydraJobs.host3".into()
            ],
            only: None,
        }));
    }

    #[test]
    fn a_crashed_batch_is_split_and_a_lone_member_fails_after_a_retry() {
        let crash = anyhow::anyhow!("eval worker closed pipe: signal: 9 (SIGKILL)");
        let batch = DiscoveryCall {
            wildcards: vec!["hydraJobs.*".into(), "!hydraJobs.ap1".into()],
            only: Some(vec!["ap10".into(), "ap11".into()]),
        };
        let lone = DiscoveryCall {
            wildcards: vec!["hydraJobs.*".into()],
            only: Some(vec!["ap10".into()]),
        };
        let whole = DiscoveryCall {
            wildcards: vec!["packages.x86_64-linux.hello".into()],
            only: None,
        };

        let split = crashed_listing(&batch, 1, &crash).expect("a batch splits at once");
        assert_eq!(
            split.deferred,
            vec![DiscoveryShard {
                pattern: "hydraJobs.*".into(),
                only: Some(vec!["ap10".into(), "ap11".into()]),
            }]
        );
        assert!(split.errors.is_empty());

        assert!(crashed_listing(&lone, 1, &crash).is_none(), "retried once");
        let failed = crashed_listing(&lone, MAX_CRASH_ATTEMPTS, &crash).expect("then recorded");
        assert_eq!(failed.errors.len(), 1);
        assert_eq!(failed.errors[0].attr, "hydraJobs.ap10");
        assert!(failed.errors[0].message.contains("SIGKILL"), "{failed:?}");

        assert!(crashed_listing(&whole, MAX_CRASH_ATTEMPTS, &crash).is_none());
    }

    #[test]
    fn entry_point_longest_prefix_match() {
        let pats = vec!["packages.*.*".into(), "packages.*.foo".into()];
        assert_eq!(
            entry_point_of("packages.x86_64-linux.hello", &["packages.*.*".into()]),
            "packages.*.*"
        );
        assert_eq!(
            entry_point_of("packages.x86_64-linux.foo", &pats),
            "packages.*.foo"
        );
        assert_eq!(entry_point_of("checks.x86_64-linux.t", &pats), "checks");
        assert_eq!(
            entry_point_of(
                "devShells.aarch64-linux.default",
                &["devShells.*.default".into()]
            ),
            "devShells.*.default"
        );
    }

    #[tokio::test]
    async fn a_failed_fetch_keeps_the_worker_unless_its_pipe_broke() {
        use super::super::input_fetch::InputBoard;
        use super::super::pool::tests::replying_worker;
        use super::super::transport::EvalWorker;
        use gradient_eval::ipc::EvalResponse;
        use gradient_wire::types::InputFetchState;

        let resolver = WorkerPoolResolver::new(1, u64::MAX, String::new());
        let target = || DownloadTarget {
            board: InputBoard::new(vec![("a".into(), InputFetchState::Fetching)]),
            index: 0,
        };
        let not_found = EvalResponse::Err {
            message: "404".into(),
        };
        resolver
            .pool
            .push_for_test(replying_worker(&not_found, "fetch-404"));
        let err = resolver
            .fetch_input("{}".into(), None, target())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "eval worker: 404");
        assert_eq!(resolver.pool.idle_count(), 1);

        let mut exits = tokio::process::Command::new("sh");
        exits.arg("-c").arg("head -c 1 >/dev/null");
        resolver
            .pool
            .push_for_test(EvalWorker::from_command(exits, Arc::default()).unwrap());
        resolver
            .fetch_input("{}".into(), None, target())
            .await
            .unwrap_err();
        assert_eq!(resolver.pool.idle_count(), 1);
    }

    #[tokio::test]
    async fn pooled_fan_out_drains_all_items_and_propagates_errors() {
        let seen = Mutex::new(Vec::new());
        let seen_ref = &seen;
        pooled_fan_out(3, (0..10).collect(), |n| async move {
            seen_ref.lock().push(n);
            Ok(Vec::new())
        })
        .await
        .expect("no errors");
        let mut got = seen.into_inner();
        got.sort_unstable();
        assert_eq!(got, (0..10).collect::<Vec<_>>());

        let err = pooled_fan_out(2, vec![1, 2, 3], |n| async move {
            anyhow::ensure!(n != 2, "boom on {n}");
            Ok(Vec::new())
        })
        .await
        .expect_err("error must propagate");
        assert!(err.to_string().contains("boom on 2"));
    }

    #[tokio::test]
    async fn pooled_fan_out_runs_follow_ups_while_the_slow_item_is_in_flight() {
        let seen = Mutex::new(Vec::new());
        let seen_ref = &seen;
        pooled_fan_out(2, vec![0, 1], |n| async move {
            if n == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            seen_ref.lock().push(n);
            Ok(if n == 1 { vec![10, 11] } else { Vec::new() })
        })
        .await
        .expect("no errors");

        assert_eq!(seen.into_inner(), vec![1, 10, 11, 0]);
    }
}
