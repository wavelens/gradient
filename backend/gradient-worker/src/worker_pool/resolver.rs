/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_db::{Derivation, parse_drv};
use gradient_eval::ipc::ResolvedItem;
use gradient_exec::path_utils::nix_store_path;
use gradient_nix::{DerivationResolver, FlakeDiscovery, ResolvedDerivation};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tracing::debug;

use super::eval_stats::{EvalStatsAccumulator, EvalStatsTotals, StatsDelta};
use super::pool::{EvalWorkerPool, PooledEvalWorker};

/// `DerivationResolver` impl that drives an [`EvalWorkerPool`].
///
/// `list_flake_derivations` plans per-system shards on one worker and fans
/// them across the pool; `resolve_derivation_paths` splits attrs into batches
/// the same way. Both fan-outs run through [`pooled_fan_out`] and recover from
/// subprocess crashes with the same [`MAX_CRASH_ATTEMPTS`] tolerance: a shard
/// retries whole (its response is atomic), a resolve batch salvages its
/// streamed prefix and isolates the exact in-flight attr.
/// `get_derivation` and `get_features` parse `.drv` files directly from disk.
#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalWorkerPool>,
    eval_cache_dir: String,
    /// Accumulates per-request deltas + peak RSS across one eval. Drained by
    /// the executor via [`Self::take_eval_stats`] once the eval finishes.
    stats: Arc<Mutex<EvalStatsAccumulator>>,
    /// The eval's user entry-point patterns, set by `list_flake_derivations` so
    /// the later resolve pass can bucket each delta under its owning pattern.
    patterns: Arc<Mutex<Vec<String>>>,
    /// Warning sink for the resolve pass: batches funnel their `ResolveEnd`
    /// warnings here, drained once per `resolve_derivation_paths` call.
    resolve_warnings: Arc<Mutex<Vec<String>>>,
}

/// One resolved attr keyed by its index in the original request.
type IndexedDerivation = (usize, ResolvedDerivation);

/// Crashes tolerated for a single work item (a shard, or one attr) before it
/// becomes an error. Shared by listing and resolving so both recover from a
/// subprocess death with the same tolerance.
const MAX_CRASH_ATTEMPTS: u32 = 2;

/// Upper bound on attrs resolved in a single worker call, so one batch's
/// eval-heap growth stays small relative to `max_eval_rss` (the worker's heap
/// persists across batches and is recycled once it crosses the cap).
const MAX_RESOLVE_BATCH: usize = 64;

/// Pick `attr`'s owning entry-point: the longest wildcard `pattern` whose
/// segments all match `attr`'s leading segments (a `*` segment matches any one
/// segment). Falls back to `attr`'s top-level segment when nothing matches.
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

    // Rank by segment count, then prefer fewer `*` (the more specific pattern).
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

/// Convert a worker's [`ResolvedItem`] into the trait's `(attr, Result)` shape.
fn item_to_resolved(item: ResolvedItem) -> ResolvedDerivation {
    let result = match (item.drv_path, item.error) {
        (Some(drv), _) => Ok((drv, item.references)),
        (None, Some(msg)) => Err(anyhow::anyhow!(msg)),
        (None, None) => Err(anyhow::anyhow!("eval worker returned empty result")),
    };

    (item.attr, result)
}

/// Per-attr error recorded when a subprocess crashes on the same attr
/// [`MAX_CRASH_ATTEMPTS`] times.
fn crashed_derivation(attr: String) -> ResolvedDerivation {
    (
        attr,
        Err(anyhow::anyhow!(
            "evaluator crashed while resolving this attribute"
        )),
    )
}

/// Dynamic work queue over the pool: `workers` loops each pull the next item
/// as soon as they are free, so one slow item never leaves the rest of the
/// pool idle. The first hard error drains the fan-out and propagates.
async fn pooled_fan_out<T, Fut>(
    workers: usize,
    items: Vec<T>,
    run: impl Fn(T) -> Fut + Sync,
) -> Result<()>
where
    T: Send,
    Fut: Future<Output = Result<()>>,
{
    let queue = Mutex::new(VecDeque::from(items));
    let (queue, run) = (&queue, &run);
    let mut tasks: FuturesUnordered<_> = (0..workers.max(1))
        .map(|_| async move {
            loop {
                // Scope the pop so the guard drops before the await; holding
                // it across `.await` would deadlock the executor.
                let item = queue.lock().unwrap().pop_front();
                let Some(item) = item else { break };
                run(item).await?;
            }

            Ok::<(), anyhow::Error>(())
        })
        .collect();

    while let Some(drained) = tasks.next().await {
        drained?;
    }

    Ok(())
}

/// Outcome of resolving one batch on one worker. `Complete` means the
/// subprocess lived through `ResolveEnd` (per-attr eval errors ride inside
/// the items); `Crashed` means it died mid-stream, keeping the item frames
/// that made it out.
enum BatchCall {
    Complete(Vec<ResolvedItem>),
    Crashed { streamed: Vec<ResolvedItem> },
}

/// Resolves one batch of attrs on a single pooled worker. Injected so the
/// crash-isolation policy is testable without real subprocesses.
type ResolveOnce<'a> = dyn Fn(Vec<String>) -> BoxFuture<'a, Result<BatchCall>> + Sync + 'a;

/// Pure crash-isolation policy over the streamed `Resolve` protocol.
///
/// A crash keeps every item streamed before the subprocess died; the first
/// unstreamed attr is the one that was in flight, so it is retried alone on a
/// fresh worker (up to [`MAX_CRASH_ATTEMPTS`] attempts total, then a per-attr
/// error) while the untouched remainder resolves independently. No bisection:
/// streaming pinpoints the suspect in one step.
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
                    // Died between the last item and ResolveEnd: every attr
                    // resolved; only the batch's warnings/stats are lost.
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

    /// Arm the pool's memory guard and spawn the eval-subprocess reaper. The
    /// margin is shared between the reaper (which kills the largest eval under
    /// pressure) and `acquire` back-pressure. No-op when `min_free_bytes` is 0
    /// or no tokio runtime is available (e.g. in unit tests).
    pub fn start_memory_reaper(&self, min_free_bytes: u64) {
        self.pool.configure_memory_guard(min_free_bytes);
        if min_free_bytes == 0 || tokio::runtime::Handle::try_current().is_err() {
            return;
        }

        let weak = Arc::downgrade(&self.pool);
        tokio::spawn(super::memory::memory_reaper_loop(weak, min_free_bytes));
    }

    /// Bucket one worker delta under `entry_point`, folding peak RSS in too.
    fn observe_stats(&self, entry_point: &str, delta: StatsDelta, rss: u64) {
        self.stats.lock().unwrap().observe(entry_point, delta, rss);
    }

    /// Post-call bookkeeping shared by every successful worker call: record
    /// the stats delta and discard an over-RSS worker so its eval heap is
    /// reclaimed before the next call (progress is durable in the eval cache).
    fn finish_call(&self, worker: &mut PooledEvalWorker, bucket: &str, stats: Option<StatsDelta>) {
        let rss = worker.rss_bytes();
        if let Some(delta) = stats {
            self.observe_stats(bucket, delta, rss);
        }
        if rss > self.pool.max_eval_rss() {
            worker.mark_dead();
        }
    }

    /// The entry-point bucket a call's stats delta is attributed to: the batch
    /// delta is one number, so it goes whole to the first attr's entry-point;
    /// the dynamic queue keeps batches small and same-prefix, so cross-bucket
    /// bleed is minor.
    fn bucket_of(&self, first_attr: Option<&String>) -> String {
        first_attr
            .map(|a| entry_point_of(a, &self.patterns.lock().unwrap()))
            .unwrap_or_default()
    }

    /// Drain the accumulated per-eval stats, resetting the accumulator so the
    /// next eval starts clean. Called once by the executor at eval completion.
    pub fn take_eval_stats(&self) -> EvalStatsTotals {
        let acc = std::mem::take(&mut *self.stats.lock().unwrap());
        acc.finish()
    }

    /// On-disk eval-cache directory shared by every worker (set as
    /// `NIX_CACHE_HOME`). The executor stages/reads `<fingerprint>.sqlite`
    /// blobs under `<eval_cache_dir>/eval-cache-v6/`.
    pub fn eval_cache_dir(&self) -> &str {
        &self.eval_cache_dir
    }

    /// Gracefully shut every idle eval-worker subprocess down. See
    /// [`EvalWorkerPool::shutdown`] for the contract.
    pub async fn shutdown(&self) {
        self.pool.shutdown().await;
    }

    /// Return `repository`'s eval-cache fingerprint without evaluating it.
    /// `None` for mutable/dirty flakes. A dead worker is marked so it gets
    /// discarded instead of reused.
    pub async fn fingerprint(&self, repository: String) -> Result<Option<String>> {
        let mut worker = self.pool.acquire().await?;
        match worker.fingerprint(repository).await {
            Ok(v) => Ok(v),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    /// Fold the eval-cache WAL into the main `.sqlite` once, after all shards
    /// have committed, so the fleet-share push ships a complete cache. Best-effort
    /// in spirit (the caller ignores failures), but a crashed worker is marked
    /// dead so it is not reused.
    pub async fn checkpoint_cache(&self, repository: String) -> Result<()> {
        let mut worker = self.pool.acquire().await?;
        match worker.checkpoint(repository).await {
            Ok(()) => Ok(()),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    /// Discover one shard, retrying on a fresh worker after a subprocess crash
    /// up to [`MAX_CRASH_ATTEMPTS`] total attempts (the resolve-side tolerance).
    /// A shard's response is atomic, so recovery is a whole-shard retry; a
    /// shard that keeps crashing fails the whole listing, matching the
    /// single-worker behaviour this replaces.
    async fn list_shard(
        &self,
        repository: &str,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
        let mut attempt = 0;
        loop {
            match self.list_once(repository, wildcards.clone()).await {
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

    /// Discover one shard on a single pooled worker; a crash marks the worker
    /// dead and surfaces as `Err` for [`Self::list_shard`] to retry.
    async fn list_once(
        &self,
        repository: &str,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
        let bucket = self.bucket_of(wildcards.first());
        let mut worker = self.pool.acquire().await?;
        match worker.list(repository.to_string(), wildcards).await {
            Ok((attrs, warnings, errors, stats)) => {
                self.finish_call(&mut worker, &bucket, stats);
                Ok((attrs, warnings, errors))
            }
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    /// Resolve one batch on a single pooled worker. A subprocess death becomes
    /// [`BatchCall::Crashed`] carrying the streamed prefix; only pool failures
    /// (e.g. shutdown) are `Err`.
    async fn resolve_once(&self, repository: &str, attrs: Vec<String>) -> Result<BatchCall> {
        let bucket = self.bucket_of(attrs.first());
        let mut worker = self.pool.acquire().await?;
        let (items, end) = worker.resolve(repository.to_string(), attrs).await;
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
            self.resolve_warnings.lock().unwrap().extend(warnings);
        }
    }
}

#[async_trait]
impl DerivationResolver for WorkerPoolResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<FlakeDiscovery> {
        // Record the user entry-points so the resolve pass can bucket by them.
        *self.patterns.lock().unwrap() = wildcards
            .iter()
            .filter(|w| !w.starts_with('!'))
            .cloned()
            .collect();

        // Plan the split on one worker (cheap: forces only the prefix attrset),
        // then discover each shard separately. A single giant discovery of the
        // whole flake is the call that blows past the RAM budget and never
        // returns; one shard per system keeps each worker within budget and lets
        // discovery advance (and persist) system-by-system.
        let (shards, plan_errors) = {
            let mut worker = self.pool.acquire().await?;
            match worker.plan(repository.clone(), wildcards.clone()).await {
                Ok(v) => v,
                Err(e) => {
                    worker.mark_dead();
                    return Err(e);
                }
            }
        };

        tracing::info!(
            shards = shards.len(),
            pool = self.pool.max(),
            "discovery split into per-system shards"
        );

        // Nothing to fan out: one pass, plus the plan-phase errors.
        if shards.len() <= 1 {
            let (attrs, warnings, mut errors) = self.list_shard(&repository, wildcards).await?;
            errors.extend(plan_errors);
            errors.sort_unstable();
            errors.dedup();
            return Ok(FlakeDiscovery { attrs, warnings, errors });
        }

        // Exact-path exclusions ride every shard; the final dedup mops up any
        // cross-shard overlap.
        let excludes: Vec<String> = wildcards
            .iter()
            .filter(|w| w.starts_with('!'))
            .cloned()
            .collect();

        let attrs = Mutex::new(Vec::<String>::new());
        let warnings = Mutex::new(Vec::<String>::new());
        let errors = Mutex::new(plan_errors);
        {
            let repo = repository.as_str();
            let (attrs, warnings, errors, excludes) = (&attrs, &warnings, &errors, &excludes);
            pooled_fan_out(self.pool.max(), shards, |shard| async move {
                let mut pattern = Vec::with_capacity(1 + excludes.len());
                pattern.push(shard);
                pattern.extend_from_slice(excludes);

                let (a, w, e) = self.list_shard(repo, pattern).await?;
                attrs.lock().unwrap().extend(a);
                warnings.lock().unwrap().extend(w);
                errors.lock().unwrap().extend(e);
                Ok(())
            })
            .await?;
        }

        let mut attrs = attrs.into_inner().unwrap();
        attrs.sort_unstable();
        attrs.dedup();
        let mut warnings = warnings.into_inner().unwrap();
        warnings.sort_unstable();
        warnings.dedup();
        let mut errors = errors.into_inner().unwrap();
        errors.sort_unstable();
        errors.dedup();

        Ok(FlakeDiscovery { attrs, warnings, errors })
    }

    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedDerivation>, Vec<String>)> {
        if attrs.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Dynamic work queue: split into many small index-tagged batches and let
        // each pooled worker pull the next as soon as it is free. ~4 batches per
        // worker leaves enough slack to steal without paying a walker rebuild
        // per attr; the size cap bounds one batch's eval-heap overshoot past
        // `max_eval_rss` (the heap persists across batches, so the cap bounds
        // the per-call overshoot, not the base cost).
        let n_workers = self.pool.max().min(attrs.len());
        let batch_size = attrs
            .len()
            .div_ceil(n_workers * 4)
            .clamp(1, MAX_RESOLVE_BATCH);
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
            let resolve_batch =
                move |attrs: Vec<String>| -> BoxFuture<'_, Result<BatchCall>> {
                    Box::pin(self.resolve_once(repo, attrs))
                };

            let (indexed, resolve_batch) = (&indexed, &resolve_batch);
            pooled_fan_out(n_workers, batches, |batch| async move {
                // A crash became per-attr errors in the pure core; only a
                // protocol violation or pool failure is a hard error.
                let resolved = resolve_chunk(resolve_batch, batch, 0).await?;
                indexed.lock().unwrap().extend(resolved);
                Ok(())
            })
            .await?;
        }

        let mut indexed = indexed.into_inner().unwrap();
        indexed.sort_by_key(|(idx, _)| *idx);
        let mut all_warnings = std::mem::take(&mut *self.resolve_warnings.lock().unwrap());
        all_warnings.sort_unstable();
        all_warnings.dedup();

        Ok((indexed.into_iter().map(|(_, r)| r).collect(), all_warnings))
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
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    #[test]
    fn entry_point_longest_prefix_match() {
        let pats = vec!["packages.*.*".into(), "packages.*.foo".into()];
        assert_eq!(
            entry_point_of("packages.x86_64-linux.hello", &["packages.*.*".into()]),
            "packages.*.*"
        );
        // A more specific literal pattern wins over the broad wildcard.
        assert_eq!(
            entry_point_of("packages.x86_64-linux.foo", &pats),
            "packages.*.foo"
        );
        // No matching pattern falls back to the attr's top-level segment.
        assert_eq!(entry_point_of("checks.x86_64-linux.t", &pats), "checks");
        // A `*` segment matches any one segment.
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

    /// Scripts a `resolve_once` stub: counts calls and decides per batch where
    /// the worker crashes. `crash_at` returns the index of the attr the worker
    /// dies on (items before it stream out), or `None` for a complete batch.
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
            *self.calls.lock().unwrap()
        }
    }

    /// Crash while resolving any attr in `crashers`, streaming everything
    /// before it (mirrors the real subprocess: items flush per attr).
    fn crashes_on(crashers: &'static [&'static str]) -> Stub {
        let set: HashSet<&str> = crashers.iter().copied().collect();
        Stub::new(move |attrs, _call| attrs.iter().position(|a| set.contains(a.as_str())))
    }

    /// Drive the pure core over a chunk built from `attrs` (index = position).
    /// Returns `(attr, resolved_ok)` in index order plus the stub's call count.
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
                        let mut n = stub.calls.lock().unwrap();
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
        // b always crashes the worker: the streamed prefix (a) survives the
        // first call, b is retried alone and errors, c+d resolve untouched.
        let (out, calls) = run(crashes_on(&["b"]), &["a", "b", "c", "d"]).await;
        let map: HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"], "the crasher resolves to an error");
        assert!(map["a"] && map["c"] && map["d"], "the rest still resolve");
        // 1 initial + 1 lone retry of b + 1 for the remainder [c, d]: streaming
        // pinpoints the suspect, no bisection rework of the streamed prefix.
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
        // The single-attr chunk crashes on the first call (attempt 0) and
        // resolves on the one retry (attempt 1) - no per-attr error recorded.
        let (out, calls) = run(
            Stub::new(|_attrs, call| (call == 0).then_some(0)),
            &["a"],
        )
        .await;
        assert_eq!(out, vec![("a".into(), true)]);
        assert_eq!(calls, 2, "one crash + one successful retry");
    }

    #[tokio::test]
    async fn crash_after_last_item_keeps_all_results() {
        // Worker streams every item, then dies before ResolveEnd: nothing to
        // retry, all results are kept.
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
            seen_ref.lock().unwrap().push(n);
            Ok(())
        })
        .await
        .expect("no errors");
        let mut got = seen.into_inner().unwrap();
        got.sort_unstable();
        assert_eq!(got, (0..10).collect::<Vec<_>>());

        let err = pooled_fan_out(2, vec![1, 2, 3], |n| async move {
            anyhow::ensure!(n != 2, "boom on {n}");
            Ok(())
        })
        .await
        .expect_err("error must propagate");
        assert!(err.to_string().contains("boom on 2"));
    }
}
