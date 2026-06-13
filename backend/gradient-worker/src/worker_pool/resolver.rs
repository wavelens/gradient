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
use std::sync::Arc;

use super::pool::EvalWorkerPool;
use crate::nix::eval_worker::ResolvedItem;

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
}

/// One resolved attr keyed by its index in the original request.
type IndexedDerivation = (usize, ResolvedDerivation);

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
        }
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

    /// Resolve `attrs` on one fresh worker. `Ok` means the subprocess lived
    /// (per-attr eval errors ride inside the items); `Err` means it crashed.
    /// An over-RSS worker is discarded after a successful call so the next
    /// `acquire` spawns a fresh subprocess.
    async fn resolve_once(
        &self,
        repository: &str,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedItem>, Vec<String>)> {
        let mut worker = self.pool.acquire().await?;
        match worker.resolve(repository.to_string(), attrs).await {
            Ok((items, warnings)) => {
                if worker.rss_bytes() > self.pool.max_eval_rss() {
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
        let mut worker = self.pool.acquire().await?;
        match worker.list(repository, wildcards).await {
            Ok(v) => Ok(v),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }

    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedDerivation>, Vec<String>)> {
        if attrs.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Round-robin partition into one chunk per worker, preserving the
        // original index so we can re-order at the end.
        let n_workers = self.pool.max().min(attrs.len());
        let mut chunks: Vec<Vec<(usize, String)>> = (0..n_workers).map(|_| Vec::new()).collect();
        for (idx, a) in attrs.into_iter().enumerate() {
            chunks[idx % n_workers].push((idx, a));
        }

        // Shared warning sink: the pure core's `Ok` payload is items only, so
        // warnings are funnelled here instead of through `resolve_chunk`. The
        // closure (and its `&warnings` borrow) is dropped with the block, so
        // `warnings` can be unwrapped by value afterwards.
        let warnings = std::sync::Mutex::new(Vec::<String>::new());
        let mut indexed: Vec<IndexedDerivation> = Vec::new();
        {
            let repo = repository.as_str();
            let sink = &warnings;
            let resolve_batch = move |attrs: Vec<String>| -> BoxFuture<'_, Result<Vec<ResolvedItem>>> {
                Box::pin(async move {
                    let (items, w) = self.resolve_once(repo, attrs).await?;
                    sink.lock().unwrap().extend(w);
                    Ok(items)
                })
            };

            let mut tasks: FuturesUnordered<_> = chunks
                .into_iter()
                .filter(|c| !c.is_empty())
                .map(|chunk| resolve_chunk(&resolve_batch, chunk, 0))
                .collect();

            // A crash became per-attr errors in the core; only a protocol
            // violation (item-count mismatch) propagates as a hard failure.
            while let Some(chunk_result) = tasks.next().await {
                indexed.extend(chunk_result?);
            }
        }

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
    struct Stub {
        calls: Mutex<usize>,
        crash_if: Box<dyn Fn(&[String], usize) -> bool + Sync>,
    }

    impl Stub {
        fn new(crash_if: impl Fn(&[String], usize) -> bool + Sync + 'static) -> Self {
            Self {
                calls: Mutex::new(0),
                crash_if: Box::new(crash_if),
            }
        }

        fn resolve_once(&self) -> impl Fn(Vec<String>) -> BoxFuture<'_, Result<Vec<ResolvedItem>>> + Sync {
            // The future borrows `self`, tying its lifetime to the stub so it
            // coerces to `&ResolveOnce<'_>` (a non-`'static` `&dyn Fn`).
            move |attrs: Vec<String>| {
                Box::pin(async move {
                    let prior = {
                        let mut n = self.calls.lock().unwrap();
                        let prior = *n;
                        *n += 1;
                        prior
                    };
                    if (self.crash_if)(&attrs, prior) {
                        anyhow::bail!("worker crashed");
                    }

                    Ok(attrs.iter().map(|a| ok_item(a)).collect())
                })
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
    async fn run(stub: &Stub, attrs: &[&str]) -> Vec<(String, bool)> {
        let resolve_once = stub.resolve_once();
        let chunk: Vec<(usize, String)> = attrs
            .iter()
            .enumerate()
            .map(|(i, a)| (i, a.to_string()))
            .collect();
        let mut out = resolve_chunk(&resolve_once, chunk, 0).await.unwrap();
        out.sort_by_key(|(idx, _)| *idx);
        out.into_iter()
            .map(|(_, (attr, r))| (attr, r.is_ok()))
            .collect()
    }

    #[tokio::test]
    async fn no_crash_resolves_all_in_one_call() {
        let stub = crashes_on(&[]);
        let out = run(&stub, &["a", "b", "c"]).await;
        assert_eq!(out, vec![
            ("a".into(), true),
            ("b".into(), true),
            ("c".into(), true),
        ]);
        assert_eq!(stub.calls(), 1, "no crash → single batch call");
    }

    #[tokio::test]
    async fn single_crasher_among_healthy_isolates_one_error() {
        let stub = crashes_on(&["b"]);
        let out = run(&stub, &["a", "b", "c", "d"]).await;
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"], "the crasher resolves to an error");
        assert!(map["a"] && map["c"] && map["d"], "the rest still resolve");
    }

    #[tokio::test]
    async fn two_crashers_isolate_independently() {
        let stub = crashes_on(&["b", "d"]);
        let out = run(&stub, &["a", "b", "c", "d", "e"]).await;
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert!(!map["b"] && !map["d"], "both crashers error");
        assert!(map["a"] && map["c"] && map["e"], "the rest resolve");
    }

    #[tokio::test]
    async fn transient_crash_succeeds_on_retry() {
        // A single-attr chunk crashes on the first call (attempt 0) and resolves
        // on the one retry (attempt 1) - no per-attr error is recorded.
        let stub = Stub::new(|_attrs, call| call == 0);
        let out = run(&stub, &["a"]).await;
        assert_eq!(out, vec![("a".into(), true)]);
        assert_eq!(stub.calls(), 2, "one crash + one successful retry");
    }
}
