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
use gradient_nix::{DerivationResolver, ResolvedDerivation};
use std::sync::{Arc, Mutex};

use super::pool::EvalWorkerPool;
use crate::nix::eval_worker::ResolvedItem;
use crate::worker_pool::eval_stats::{EvalStatsAccumulator, EvalStatsTotals, StatsDelta};

/// Strips `/nix/store/` and returns just the hash-name component (mirrors the
/// helper in [`super::resolver`]).
fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

/// `DerivationResolver` impl that drives an [`EvalWorkerPool`].
///
/// `list_flake_derivations` is dispatched to a single worker (the wildcard
/// `*` is recursive in the cursor walk, so one call discovers all depths).
/// `resolve_derivation_paths` splits attrs across the pool for parallelism and
/// bisects on subprocess crash so one bad attr never fails the whole eval.
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
}

/// One resolved attr keyed by its index in the original request.
type IndexedDerivation = (usize, ResolvedDerivation);

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

/// Per-attr error recorded when a subprocess crashes twice on the same attr.
fn crashed_derivation(attr: String) -> ResolvedDerivation {
    (
        attr,
        Err(anyhow::anyhow!(
            "evaluator crashed while resolving this attribute"
        )),
    )
}

/// Resolves one batch of attrs on a single fresh worker. `Ok` means the
/// subprocess lived (per-attr eval errors ride inside the items); `Err` means
/// it crashed mid-batch. Injected so the crash-isolation policy is testable
/// without real subprocesses.
type ResolveOnce<'a> = dyn Fn(Vec<String>) -> BoxFuture<'a, Result<Vec<ResolvedItem>>> + Sync + 'a;

/// Crashes tolerated for a single attr before it becomes a per-attr error.
const MAX_CRASH_ATTEMPTS: u32 = 2;

/// Upper bound on attrs resolved in a single worker call, so one batch's
/// eval-heap growth stays small relative to `max_eval_rss` (the worker's heap
/// persists across batches and is recycled once it crosses the cap).
const MAX_RESOLVE_BATCH: usize = 64;

/// Pure crash-isolation policy. Resolves `chunk` via `resolve_once`; an `Err`
/// (subprocess crash, never a per-attr eval failure) is recovered by bisection:
/// split a multi-attr chunk in half so the crasher is isolated in `O(log n)`
/// while the rest resolve, and retry a single attr once before recording a
/// per-attr error. `attempt` counts crashes for a single-attr chunk, capped at
/// [`MAX_CRASH_ATTEMPTS`].
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
        match resolve_once(attrs).await {
            Ok(items) => {
                if items.len() != chunk.len() {
                    anyhow::bail!(
                        "eval worker returned {} items for {} attrs",
                        items.len(),
                        chunk.len()
                    );
                }

                Ok(chunk
                    .into_iter()
                    .zip(items)
                    .map(|((idx, _attr), item)| (idx, item_to_resolved(item)))
                    .collect())
            }
            Err(_crash) if chunk.len() == 1 => {
                let (idx, attr) = chunk.into_iter().next().expect("len == 1");
                if attempt + 1 >= MAX_CRASH_ATTEMPTS {
                    return Ok(vec![(idx, crashed_derivation(attr))]);
                }

                resolve_chunk(resolve_once, vec![(idx, attr)], attempt + 1).await
            }
            Err(_crash) => {
                let right = chunk.split_off(chunk.len() / 2);
                let (mut a, b) = futures::future::try_join(
                    resolve_chunk(resolve_once, chunk, 0),
                    resolve_chunk(resolve_once, right, 0),
                )
                .await?;
                a.extend(b);
                Ok(a)
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
        }
    }

    /// Bucket one worker delta under `entry_point`, folding peak RSS in too.
    fn observe_stats(&self, entry_point: &str, delta: StatsDelta, rss: u64) {
        self.stats.lock().unwrap().observe(entry_point, delta, rss);
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
    /// `None` for mutable/dirty flakes. Mirrors [`Self::list_flake_derivations`]:
    /// a dead worker is marked so it gets discarded instead of reused.
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

    /// Discover one shard on a pooled worker, retrying once on a fresh worker
    /// if the subprocess crashes (a transient stack/OOM death). A shard that
    /// crashes twice propagates the error, failing the whole listing - matching
    /// the single-worker behaviour this replaces.
    async fn list_shard(
        &self,
        repository: &str,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        match self.list_once(repository, wildcards.clone()).await {
            Ok(v) => Ok(v),
            Err(_crash) => self.list_once(repository, wildcards).await,
        }
    }

    /// Discover one shard on a single worker. An over-RSS worker is discarded
    /// after a successful call so its eval heap is reclaimed before the next
    /// shard (progress is already durable in the eval cache); a crash marks the
    /// worker dead. Mirrors [`Self::resolve_once`].
    async fn list_once(
        &self,
        repository: &str,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let bucket = wildcards
            .first()
            .map(|w| entry_point_of(w, &self.patterns.lock().unwrap()))
            .unwrap_or_default();
        let mut worker = self.pool.acquire().await?;
        match worker.list(repository.to_string(), wildcards).await {
            Ok((attrs, warnings, stats)) => {
                let rss = worker.rss_bytes();
                if let Some(delta) = stats {
                    self.observe_stats(&bucket, delta, rss);
                }
                if rss > self.pool.max_eval_rss() {
                    worker.mark_dead();
                }

                Ok((attrs, warnings))
            }
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    /// Resolve `attrs` on one fresh worker. `Ok` means the subprocess lived
    /// (per-attr eval errors ride inside the items); `Err` means it crashed.
    /// An over-RSS worker is discarded after a successful call so the next
    /// `acquire` spawns a fresh subprocess.
    async fn resolve_once(
        &self,
        repository: &str,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedItem>, Vec<String>)> {
        // A batch's delta is one number, so it is attributed whole to the
        // entry-point of its first attr; the dynamic queue keeps batches small
        // and same-prefix, so cross-bucket bleed is minor.
        let bucket = attrs
            .first()
            .map(|a| entry_point_of(a, &self.patterns.lock().unwrap()))
            .unwrap_or_default();
        let mut worker = self.pool.acquire().await?;
        match worker.resolve(repository.to_string(), attrs).await {
            Ok((items, warnings, stats)) => {
                let rss = worker.rss_bytes();
                if let Some(delta) = stats {
                    self.observe_stats(&bucket, delta, rss);
                }
                if rss > self.pool.max_eval_rss() {
                    worker.mark_dead();
                }

                Ok((items, warnings))
            }
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }
}

#[async_trait]
impl DerivationResolver for WorkerPoolResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
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
        let shards = {
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

        // Nothing to fan out (a wildcard-free or single-child pattern): one pass.
        if shards.len() <= 1 {
            return self.list_shard(&repository, wildcards).await;
        }

        // Exact-path exclusions ride every shard; the final dedup mops up any
        // cross-shard overlap.
        let excludes: Vec<String> = wildcards
            .iter()
            .filter(|w| w.starts_with('!'))
            .cloned()
            .collect();

        let queue: std::sync::Mutex<std::collections::VecDeque<String>> =
            std::sync::Mutex::new(shards.into_iter().collect());
        let attrs = std::sync::Mutex::new(Vec::<String>::new());
        let warnings = std::sync::Mutex::new(Vec::<String>::new());
        {
            let repo = repository.as_str();
            let (queue, attrs, warnings, excludes) = (&queue, &attrs, &warnings, &excludes);
            let mut tasks: FuturesUnordered<_> = (0..self.pool.max())
                .map(|_| async move {
                    loop {
                        let shard = queue.lock().unwrap().pop_front();
                        let Some(shard) = shard else { break };

                        let mut pattern = Vec::with_capacity(1 + excludes.len());
                        pattern.push(shard);
                        pattern.extend_from_slice(excludes);

                        let (a, w) = self.list_shard(repo, pattern).await?;
                        attrs.lock().unwrap().extend(a);
                        warnings.lock().unwrap().extend(w);
                    }

                    Ok::<(), anyhow::Error>(())
                })
                .collect();

            while let Some(drained) = tasks.next().await {
                drained?;
            }
        }

        let mut attrs = attrs.into_inner().unwrap();
        attrs.sort_unstable();
        attrs.dedup();
        let mut warnings = warnings.into_inner().unwrap();
        warnings.sort_unstable();
        warnings.dedup();

        Ok((attrs, warnings))
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
        // each pooled worker pull the next as soon as it is free. A slow attr on
        // one worker no longer leaves the others idle the way the old static
        // round-robin partition did. ~4 batches per worker leaves enough slack to
        // steal without paying a walker rebuild per attr, but the size is capped
        // so one batch's eval-heap growth cannot blow a worker far past
        // `max_eval_rss` before the post-call recycle check sees it (the heap is
        // persistent across batches, so the cap bounds the per-call overshoot,
        // not the base cost).
        let n_workers = self.pool.max().min(attrs.len());
        let batch_size = attrs
            .len()
            .div_ceil(n_workers * 4)
            .clamp(1, MAX_RESOLVE_BATCH);
        let queue: std::sync::Mutex<std::collections::VecDeque<Vec<(usize, String)>>> =
            std::sync::Mutex::new(
                attrs
                    .into_iter()
                    .enumerate()
                    .collect::<Vec<_>>()
                    .chunks(batch_size)
                    .map(|c| c.to_vec())
                    .collect(),
            );

        // Shared warning sink: the pure core's `Ok` payload is items only, so
        // warnings are funnelled here instead of through `resolve_chunk`.
        let warnings = std::sync::Mutex::new(Vec::<String>::new());
        let indexed = std::sync::Mutex::new(Vec::<IndexedDerivation>::new());
        {
            let repo = repository.as_str();
            let sink = &warnings;
            let resolve_batch =
                move |attrs: Vec<String>| -> BoxFuture<'_, Result<Vec<ResolvedItem>>> {
                    Box::pin(async move {
                        let (items, w) = self.resolve_once(repo, attrs).await?;
                        sink.lock().unwrap().extend(w);
                        Ok(items)
                    })
                };

            let (queue, indexed, resolve_batch) = (&queue, &indexed, &resolve_batch);
            let mut tasks: FuturesUnordered<_> = (0..n_workers)
                .map(|_| async move {
                    loop {
                        // Scope the pop so the guard drops before the await;
                        // holding it across `.await` would deadlock the executor.
                        let batch = queue.lock().unwrap().pop_front();
                        let Some(batch) = batch else { break };

                        // A crash became per-attr errors in the core; only a
                        // protocol violation (item-count mismatch) is a hard fail.
                        let resolved = resolve_chunk(resolve_batch, batch, 0).await?;
                        indexed.lock().unwrap().extend(resolved);
                    }

                    Ok::<(), anyhow::Error>(())
                })
                .collect();

            while let Some(drained) = tasks.next().await {
                drained?;
            }
        }

        let mut indexed = indexed.into_inner().unwrap();
        indexed.sort_by_key(|(idx, _)| *idx);
        let mut all_warnings = warnings.into_inner().unwrap();
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
            return Ok(("builtin".to_string(), vec![]));
        }
        let drv = self.get_derivation(drv_path).await?;
        let features = drv.required_system_features();
        Ok((drv.system.clone(), features))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[test]
    fn nix_store_path_absolute_unchanged() {
        let path = "/nix/store/hash-name";
        assert_eq!(nix_store_path(path), path);
    }

    #[test]
    fn nix_store_path_bare_prefixed() {
        assert_eq!(nix_store_path("hash-name"), "/nix/store/hash-name");
    }

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

    /// Scripts a `resolve_once` stub: counts calls and decides per batch whether
    /// the worker crashes. `crash_if` returns true when the batch should die.
    /// `calls` is borrowed by the returned futures, tying their lifetime to the
    /// stub so it coerces to `&ResolveOnce<'_>` cleanly.
    type CrashIf = Box<dyn Fn(&[String], usize) -> bool + Sync>;

    struct Stub {
        calls: Mutex<usize>,
        crash_if: CrashIf,
    }

    impl Stub {
        fn new(crash_if: impl Fn(&[String], usize) -> bool + Sync + 'static) -> Self {
            Self {
                calls: Mutex::new(0),
                crash_if: Box::new(crash_if),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    /// Crash any batch that contains one of `crashers`.
    fn crashes_on(crashers: &'static [&'static str]) -> Stub {
        let set: HashSet<&str> = crashers.iter().copied().collect();
        Stub::new(move |attrs, _call| attrs.iter().any(|a| set.contains(a.as_str())))
    }

    /// Drive the pure core over a chunk built from `attrs` (index = position).
    /// Owns the stub so the closure and its futures share one scope (mirroring
    /// the production block); returns the result plus the stub's call count.
    async fn run(stub: Stub, attrs: &[&str]) -> (Vec<(String, bool)>, usize) {
        let chunk: Vec<(usize, String)> = attrs
            .iter()
            .enumerate()
            .map(|(i, a)| (i, a.to_string()))
            .collect();

        let mut out = {
            let stub = &stub;
            let resolve_once =
                move |attrs: Vec<String>| -> BoxFuture<'_, Result<Vec<ResolvedItem>>> {
                    Box::pin(async move {
                        let prior = {
                            let mut n = stub.calls.lock().unwrap();
                            let prior = *n;
                            *n += 1;
                            prior
                        };
                        if (stub.crash_if)(&attrs, prior) {
                            anyhow::bail!("worker crashed");
                        }

                        Ok(attrs.iter().map(|a| ok_item(a)).collect())
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
            vec![("a".into(), true), ("b".into(), true), ("c".into(), true),]
        );
        assert_eq!(calls, 1, "no crash → single batch call");
    }

    #[tokio::test]
    async fn single_crasher_among_healthy_isolates_one_error() {
        let (out, _) = run(crashes_on(&["b"]), &["a", "b", "c", "d"]).await;
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"], "the crasher resolves to an error");
        assert!(map["a"] && map["c"] && map["d"], "the rest still resolve");
    }

    #[tokio::test]
    async fn two_crashers_isolate_independently() {
        let (out, _) = run(crashes_on(&["b", "d"]), &["a", "b", "c", "d", "e"]).await;
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"] && !map["d"], "both crashers error");
        assert!(map["a"] && map["c"] && map["e"], "the rest resolve");
    }

    #[tokio::test]
    async fn transient_crash_succeeds_on_retry() {
        // A single-attr chunk crashes on the first call (attempt 0) and resolves
        // on the one retry (attempt 1) - no per-attr error is recorded.
        let (out, calls) = run(Stub::new(|_attrs, call| call == 0), &["a"]).await;
        assert_eq!(out, vec![("a".into(), true)]);
        assert_eq!(calls, 2, "one crash + one successful retry");
    }
}
