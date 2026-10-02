/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_derivation::{Derivation, parse_drv};
use gradient_eval::ipc::{DiscoveryShard, ResolvedItem};
use gradient_sources::{DerivationResolver, FlakeDiscovery, ResolvedDerivation};
use gradient_util::store_path::nix_store_path;
use gradient_util::sync::Mutex;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use tracing::debug;

use super::eval_stats::{EvalStatsAccumulator, EvalStatsTotals, StatsDelta};
use super::pool::{EvalWorkerPool, PooledEvalWorker};
use super::transport::Listing;

#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalWorkerPool>,
    eval_cache_dir: String,
    stats: Arc<Mutex<EvalStatsAccumulator>>,
    patterns: Arc<Mutex<Vec<String>>>,
    resolve_warnings: Arc<Mutex<Vec<String>>>,
}

type IndexedDerivation = (usize, ResolvedDerivation);

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

fn item_to_resolved(item: ResolvedItem) -> ResolvedDerivation {
    let result = match (item.drv_path, item.error) {
        (Some(drv), _) => Ok((drv, item.references)),
        (None, Some(msg)) => Err(anyhow::anyhow!(msg)),
        (None, None) => Err(anyhow::anyhow!("eval worker returned empty result")),
    };

    (item.attr, result)
}

fn crashed_derivation(attr: String) -> ResolvedDerivation {
    (
        attr,
        Err(anyhow::anyhow!(
            "evaluator crashed while resolving this attribute"
        )),
    )
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

enum BatchCall {
    Complete(Vec<ResolvedItem>),
    Crashed { streamed: Vec<ResolvedItem> },
}

type ResolveOnce<'a> = dyn Fn(Vec<String>) -> BoxFuture<'a, Result<BatchCall>> + Sync + 'a;

/// A crash is keeping every item streamed before the subprocess died.
/// The first unstreamed attr was in flight and is retried alone on a fresh worker.
/// The untouched remainder is resolving independently. No bisection is needed.
fn resolve_chunk<'a>(
    resolve_once: &'a ResolveOnce<'a>,
    mut chunk: Vec<(usize, String)>,
    attempt: u32,
) -> BoxFuture<'a, Result<Vec<IndexedDerivation>>> {
    Box::pin(async move {
        if chunk.is_empty() {
            return Ok(Vec::new());
        }

        let attrs: Vec<String> = chunk.iter().map(|(_, a)| a.clone()).collect();
        match resolve_once(attrs).await? {
            BatchCall::Complete(items) => {
                anyhow::ensure!(
                    items.len() == chunk.len(),
                    "eval worker returned {} items for {} attrs",
                    items.len(),
                    chunk.len()
                );

                Ok(chunk
                    .into_iter()
                    .zip(items)
                    .map(|((idx, _attr), item)| (idx, item_to_resolved(item)))
                    .collect())
            }
            BatchCall::Crashed { streamed } => {
                anyhow::ensure!(
                    streamed.len() <= chunk.len(),
                    "eval worker streamed {} items for {} attrs",
                    streamed.len(),
                    chunk.len()
                );

                let rest = chunk.split_off(streamed.len());
                for ((_, want), got) in chunk.iter().zip(&streamed) {
                    anyhow::ensure!(
                        &got.attr == want,
                        "eval worker streamed item for '{}' where '{want}' was expected",
                        got.attr
                    );
                }
                let mut done: Vec<IndexedDerivation> = chunk
                    .into_iter()
                    .zip(streamed)
                    .map(|((idx, _attr), item)| (idx, item_to_resolved(item)))
                    .collect();

                let mut rest = rest.into_iter();
                let Some((idx, suspect)) = rest.next() else {
                    return Ok(done);
                };
                let remainder: Vec<_> = rest.collect();

                if attempt + 1 >= MAX_CRASH_ATTEMPTS {
                    done.push((idx, crashed_derivation(suspect)));
                    done.extend(resolve_chunk(resolve_once, remainder, 0).await?);
                    return Ok(done);
                }

                let (retried, resolved) = futures::future::try_join(
                    resolve_chunk(resolve_once, vec![(idx, suspect)], attempt + 1),
                    resolve_chunk(resolve_once, remainder, 0),
                )
                .await?;
                done.extend(retried);
                done.extend(resolved);
                Ok(done)
            }
        }
    })
}

impl WorkerPoolResolver {
    pub fn new(pool_size: usize, max_eval_rss: u64, eval_cache_dir: String) -> Self {
        Self {
            pool: Arc::new(EvalWorkerPool::new(
                pool_size,
                max_eval_rss,
                eval_cache_dir.clone(),
            )),
            eval_cache_dir,
            stats: Arc::new(Mutex::new(EvalStatsAccumulator::default())),
            patterns: Arc::new(Mutex::new(Vec::new())),
            resolve_warnings: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn start_memory_reaper(&self, min_free_bytes: u64) {
        self.pool.configure_memory_guard(min_free_bytes);
        if min_free_bytes == 0 || tokio::runtime::Handle::try_current().is_err() {
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
                worker.mark_dead();
                Err(e)
            }
        }
    }

    async fn resolve_once(
        &self,
        repository: &str,
        attrs: Vec<String>,
        overrides: &[(String, String)],
    ) -> Result<BatchCall> {
        let bucket = self.bucket_of(attrs.first());
        let mut worker = self.pool.acquire().await?;
        let (items, end) = worker
            .resolve(repository.to_string(), attrs, overrides.to_vec())
            .await;
        match end {
            Ok((warnings, stats)) => {
                self.finish_call(&mut worker, &bucket, stats);
                self.record_warnings(warnings);
                Ok(BatchCall::Complete(items))
            }
            Err(e) => {
                worker.mark_dead();
                debug!(
                    error = format!("{e:#}"),
                    streamed = items.len(),
                    "eval worker died mid-resolve; salvaging streamed prefix"
                );
                Ok(BatchCall::Crashed { streamed: items })
            }
        }
    }

    fn record_warnings(&self, warnings: Vec<String>) {
        if !warnings.is_empty() {
            self.resolve_warnings.lock().extend(warnings);
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

        let attrs = Mutex::new(Vec::<String>::new());
        let warnings = Mutex::new(Vec::<String>::new());
        let errors = Mutex::new(plan_errors);
        {
            let repo = repository.as_str();
            let (attrs, warnings, errors) = (&attrs, &warnings, &errors);
            let excludes = excludes.as_slice();
            pooled_fan_out(self.pool.max(), calls, |call| async move {
                let listing = self.list_shard(repo, call, overrides).await?;
                attrs.lock().extend(listing.attrs);
                warnings.lock().extend(listing.warnings);
                errors.lock().extend(listing.errors);
                Ok(discovery_calls(listing.deferred, excludes, self.pool.max()))
            })
            .await?;
        }

        let mut attrs = attrs.into_inner();
        attrs.sort_unstable();
        attrs.dedup();
        let mut warnings = warnings.into_inner();
        warnings.sort_unstable();
        warnings.dedup();
        let mut errors = errors.into_inner();
        errors.sort_unstable();
        errors.dedup();

        Ok(FlakeDiscovery {
            attrs,
            warnings,
            errors,
        })
    }

    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
        overrides: &[(String, String)],
    ) -> Result<(Vec<ResolvedDerivation>, Vec<String>)> {
        if attrs.is_empty() {
            return Ok((vec![], vec![]));
        }

        let n_workers = self.pool.max().min(attrs.len());
        let batch_size = batch_size(attrs.len(), n_workers);
        let batches: Vec<Vec<(usize, String)>> = attrs
            .into_iter()
            .enumerate()
            .collect::<Vec<_>>()
            .chunks(batch_size)
            .map(|c| c.to_vec())
            .collect();

        let indexed = Mutex::new(Vec::<IndexedDerivation>::new());
        {
            let repo = repository.as_str();
            let resolve_batch = move |attrs: Vec<String>| -> BoxFuture<'_, Result<BatchCall>> {
                Box::pin(self.resolve_once(repo, attrs, overrides))
            };

            let (indexed, resolve_batch) = (&indexed, &resolve_batch);
            pooled_fan_out(n_workers, batches, |batch| async move {
                let resolved = resolve_chunk(resolve_batch, batch, 0).await?;
                indexed.lock().extend(resolved);
                Ok(Vec::new())
            })
            .await?;
        }

        let mut indexed = indexed.into_inner();
        indexed.sort_by_key(|(idx, _)| *idx);
        let mut all_warnings = std::mem::take(&mut *self.resolve_warnings.lock());
        all_warnings.sort_unstable();
        all_warnings.dedup();

        Ok((indexed.into_iter().map(|(_, r)| r).collect(), all_warnings))
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
    use std::collections::{HashMap, HashSet};

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

    fn ok_item(attr: &str) -> ResolvedItem {
        ResolvedItem {
            attr: attr.to_string(),
            drv_path: Some(format!("h-{attr}.drv")),
            references: vec![],
            error: None,
        }
    }

    type CrashAt = Box<dyn Fn(&[String], usize) -> Option<usize> + Sync>;

    struct Stub {
        calls: Mutex<usize>,
        crash_at: CrashAt,
    }

    impl Stub {
        fn new(crash_at: impl Fn(&[String], usize) -> Option<usize> + Sync + 'static) -> Self {
            Self {
                calls: Mutex::new(0),
                crash_at: Box::new(crash_at),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock()
        }
    }

    fn crashes_on(crashers: &'static [&'static str]) -> Stub {
        let set: HashSet<&str> = crashers.iter().copied().collect();
        Stub::new(move |attrs, _call| attrs.iter().position(|a| set.contains(a.as_str())))
    }

    async fn run(stub: Stub, attrs: &[&str]) -> (Vec<(String, bool)>, usize) {
        let chunk: Vec<(usize, String)> = attrs
            .iter()
            .enumerate()
            .map(|(i, a)| (i, a.to_string()))
            .collect();

        let mut out = {
            let stub = &stub;
            let resolve_once = move |attrs: Vec<String>| -> BoxFuture<'_, Result<BatchCall>> {
                Box::pin(async move {
                    let prior = {
                        let mut n = stub.calls.lock();
                        let prior = *n;
                        *n += 1;
                        prior
                    };
                    match (stub.crash_at)(&attrs, prior) {
                        Some(at) => Ok(BatchCall::Crashed {
                            streamed: attrs[..at].iter().map(|a| ok_item(a)).collect(),
                        }),
                        None => Ok(BatchCall::Complete(
                            attrs.iter().map(|a| ok_item(a)).collect(),
                        )),
                    }
                })
            };
            resolve_chunk(&resolve_once, chunk, 0).await.unwrap()
        };
        out.sort_by_key(|(idx, _)| *idx);

        let result = out
            .into_iter()
            .map(|(_, (attr, r))| (attr, r.is_ok()))
            .collect();

        (result, stub.calls())
    }

    #[tokio::test]
    async fn no_crash_resolves_all_in_one_call() {
        let (out, calls) = run(crashes_on(&[]), &["a", "b", "c"]).await;
        assert_eq!(
            out,
            vec![("a".into(), true), ("b".into(), true), ("c".into(), true)]
        );
        assert_eq!(calls, 1, "no crash means a single batch call");
    }

    #[tokio::test]
    async fn crash_salvages_streamed_prefix_and_isolates_suspect() {
        let (out, calls) = run(crashes_on(&["b"]), &["a", "b", "c", "d"]).await;
        let map: HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"], "the crasher resolves to an error");
        assert!(map["a"] && map["c"] && map["d"], "the rest still resolve");
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn two_crashers_isolate_independently() {
        let (out, _) = run(crashes_on(&["b", "d"]), &["a", "b", "c", "d", "e"]).await;
        let map: HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"] && !map["d"], "both crashers error");
        assert!(map["a"] && map["c"] && map["e"], "the rest resolve");
    }

    #[tokio::test]
    async fn transient_crash_succeeds_on_retry() {
        let (out, calls) = run(Stub::new(|_attrs, call| (call == 0).then_some(0)), &["a"]).await;
        assert_eq!(out, vec![("a".into(), true)]);
        assert_eq!(calls, 2, "one crash + one successful retry");
    }

    #[tokio::test]
    async fn crash_after_last_item_keeps_all_results() {
        let (out, calls) = run(
            Stub::new(|attrs, call| (call == 0).then_some(attrs.len())),
            &["a", "b"],
        )
        .await;
        assert_eq!(out, vec![("a".into(), true), ("b".into(), true)]);
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn streamed_attr_mismatch_is_a_protocol_error() {
        fn resolve_once(_attrs: Vec<String>) -> BoxFuture<'static, Result<BatchCall>> {
            Box::pin(async move {
                Ok(BatchCall::Crashed {
                    streamed: vec![ok_item("unrelated")],
                })
            })
        }
        let err = resolve_chunk(&resolve_once, vec![(0, "a".into()), (1, "b".into())], 0)
            .await
            .expect_err("mismatched stream must fail");
        assert!(err.to_string().contains("streamed item"), "{err}");
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
