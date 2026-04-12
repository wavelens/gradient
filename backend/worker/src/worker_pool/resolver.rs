/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_core::db::{Derivation, parse_drv};
use gradient_core::nix::{DerivationResolver, ResolvedDerivation};
use gradient_core::types::consts::FLAKE_START;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, warn};

use super::pool::EvalWorkerPool;

/// Strips `/nix/store/` and returns just the hash-name component (mirrors the
/// helper in [`super::resolver`]).
fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

/// Splits a Nix attribute path on `.`, respecting double-quoted segments.
/// Mirror of the helper in [`super::flake`] kept here so the pool does not
/// reach into a sibling module's private API.
fn split_attr_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in path.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '.' if !in_quotes => {
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    segments.push(current);
    segments
}

/// Returns the entries from `candidates` matching a pattern of the form
/// `<prefix>*<suffix>` (only one `*` supported, mirroring the limitation of
/// [`super::flake::discover_derivations`]).
fn match_pattern<'a, I>(pattern: &str, candidates: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() != 2 {
        return Vec::new();
    }
    let (start, end) = (parts[0], parts[1]);
    candidates
        .into_iter()
        .filter(|c| c.starts_with(start) && c.ends_with(end) && c.len() >= start.len() + end.len())
        .map(|c| c.to_string())
        .collect()
}

/// Wrap an attribute name in `"…"` if it contains characters that are not
/// valid in an unquoted Nix attribute path (most commonly `-` or `.`).
fn quote_if_needed(name: &str) -> String {
    let needs_quote = name
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '_'));
    if needs_quote {
        format!("\"{}\"", name)
    } else {
        name.to_string()
    }
}

/// `DerivationResolver` impl that drives an [`EvalWorkerPool`].
///
/// `list_flake_derivations` and `resolve_derivation_paths` are dispatched to
/// the pool. `get_derivation` and `get_features` parse `.drv` files directly
/// from disk and don't need the embedded evaluator at all.
#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalWorkerPool>,
}

impl WorkerPoolResolver {
    pub fn new(workers: usize, max_evaluations_per_worker: usize) -> Self {
        Self {
            pool: Arc::new(EvalWorkerPool::new(workers, max_evaluations_per_worker)),
        }
    }

    /// Splits a single wildcard into multiple, more concrete wildcards so the
    /// pool can dispatch them in parallel:
    ///
    /// - First-segment wildcards are matched against [`FLAKE_START`] (e.g. `*.*`
    ///   → `checks.*`, `packages.*`, …).
    /// - Second-segment wildcards expand by querying the systems present under
    ///   each prefix (e.g. `packages.*.*` → `packages.x86_64-linux.*`,
    ///   `packages.aarch64-linux.*`, …).
    ///
    /// On any failure during system discovery the input wildcard is kept
    /// unchanged so the worker still produces a correct (but less parallel)
    /// result.
    async fn expand_wildcards_for_pool(
        &self,
        repository: &str,
        wildcards: Vec<String>,
    ) -> Vec<String> {
        // Stage 1: expand first-segment wildcards against FLAKE_START into a
        // flat list of attr-path segment vectors.
        let mut stage1: Vec<Vec<String>> = Vec::new();
        for w in wildcards {
            let segs = split_attr_path(&w);
            if segs.is_empty() {
                stage1.push(vec![w]);
                continue;
            }

            if segs[0].contains('*') {
                let prefixes = match_pattern(&segs[0], FLAKE_START.iter().copied());
                if prefixes.is_empty() {
                    stage1.push(segs);
                } else {
                    for p in prefixes {
                        let mut v = vec![p];
                        v.extend_from_slice(&segs[1..]);
                        stage1.push(v);
                    }
                }
            } else {
                stage1.push(segs);
            }
        }

        // Stage 2: for each fragment of shape <prefix>.*.<rest>, discover the
        // systems under <prefix> and fan one wildcard out per matching system.
        // The `fetch_attr_names` calls run concurrently so independent
        // prefixes (e.g. packages / checks / devShells) don't serialize on a
        // single worker round-trip each.
        let mut discovery_tasks: FuturesUnordered<_> = FuturesUnordered::new();
        let mut passthrough: Vec<(usize, Vec<String>)> = Vec::new();

        for (idx, frag) in stage1.into_iter().enumerate() {
            if frag.len() >= 3 && frag[1].contains('*') && !frag[0].contains('*') {
                let prefix = frag[0].clone();
                let repository = repository.to_string();
                discovery_tasks.push(async move {
                    let result = self.fetch_attr_names(&repository, &prefix).await;
                    (idx, frag, result)
                });
            } else {
                passthrough.push((idx, vec![frag.join(".")]));
            }
        }

        let mut discovered: Vec<(usize, Vec<String>)> = Vec::new();
        while let Some((idx, frag, result)) = discovery_tasks.next().await {
            match result {
                Ok(systems) => {
                    let matched = match_pattern(&frag[1], systems.iter().map(String::as_str));
                    if matched.is_empty() {
                        discovered.push((idx, vec![frag.join(".")]));
                        continue;
                    }
                    let expanded: Vec<String> = matched
                        .into_iter()
                        .map(|sys| {
                            let mut v = vec![frag[0].clone(), quote_if_needed(&sys)];
                            v.extend_from_slice(&frag[2..]);
                            v.join(".")
                        })
                        .collect();
                    discovered.push((idx, expanded));
                }
                Err(e) => {
                    debug!(prefix = %frag[0], error = %e, "system discovery failed; falling back to single-wildcard fragment");
                    discovered.push((idx, vec![frag.join(".")]));
                }
            }
        }

        // Re-merge by original fragment index so output order is deterministic.
        let mut merged: Vec<(usize, Vec<String>)> =
            passthrough.into_iter().chain(discovered).collect();
        merged.sort_by_key(|(i, _)| *i);

        // De-duplicate while preserving order.
        let mut seen = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for (_, items) in merged {
            for item in items {
                if seen.insert(item.clone()) {
                    out.push(item);
                }
            }
        }
        out
    }

    /// One-shot AttrNames query against the worker pool. Acquires a worker,
    /// marks it dead on protocol failure.
    async fn fetch_attr_names(&self, repository: &str, path: &str) -> Result<Vec<String>> {
        let mut worker = self.pool.acquire().await?;
        match worker
            .attr_names(repository.to_string(), path.to_string())
            .await
        {
            Ok(v) => Ok(v),
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
        // Wildcard expansion: a bare `*` becomes
        // `*.*` and `*.*.*` so we discover both depth-2 (e.g. `formatter.<sys>`)
        // and depth-3 (e.g. `packages.<sys>.hello`) attribute paths.
        let expanded: Vec<String> = wildcards
            .into_iter()
            .flat_map(|w| {
                if w == "*" {
                    vec!["*.*".to_string(), "*.*.*".to_string()]
                } else {
                    vec![w]
                }
            })
            .collect();

        // Split wildcards by FLAKE_START prefix and (where applicable) by system
        // so we can dispatch each fragment to a separate worker in parallel.
        let fragments = self.expand_wildcards_for_pool(&repository, expanded).await;

        if fragments.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Round-robin into one chunk per worker.
        let n_workers = self.pool.max().min(fragments.len()).max(1);
        let mut chunks: Vec<Vec<String>> = (0..n_workers).map(|_| Vec::new()).collect();
        for (idx, w) in fragments.into_iter().enumerate() {
            chunks[idx % n_workers].push(w);
        }

        let mut tasks: FuturesUnordered<_> = chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|chunk| {
                let pool = Arc::clone(&self.pool);
                let repository = repository.clone();
                async move {
                    let mut worker = pool.acquire().await?;
                    match worker.list(repository, chunk).await {
                        Ok(v) => Ok(v),
                        Err(e) => {
                            worker.mark_dead();
                            Err(e)
                        }
                    }
                }
            })
            .collect();

        let mut all: HashSet<String> = HashSet::new();
        let mut all_warnings: Vec<String> = Vec::new();
        while let Some(chunk_result) = tasks.next().await {
            match chunk_result {
                Ok((items, warnings)) => {
                    all.extend(items);
                    all_warnings.extend(warnings);
                }
                Err(e) => {
                    warn!(error = %e, "eval worker list chunk failed");
                    return Err(e);
                }
            }
        }
        all_warnings.sort_unstable();
        all_warnings.dedup();
        Ok((all.into_iter().collect(), all_warnings))
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

        let mut tasks: FuturesUnordered<_> = chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|chunk| {
                let pool = Arc::clone(&self.pool);
                let repository = repository.clone();
                async move {
                    let mut worker = pool.acquire().await?;
                    let attrs_only: Vec<String> = chunk.iter().map(|(_, a)| a.clone()).collect();
                    let (items, warnings) = match worker.resolve(repository, attrs_only).await {
                        Ok(v) => v,
                        Err(e) => {
                            worker.mark_dead();
                            return Err(e);
                        }
                    };

                    // Re-stitch responses to their original indices. The worker
                    // returns items in the order it received them.
                    if items.len() != chunk.len() {
                        anyhow::bail!(
                            "eval worker returned {} items for {} attrs",
                            items.len(),
                            chunk.len()
                        );
                    }
                    let indexed: Vec<(usize, ResolvedDerivation)> = chunk
                        .into_iter()
                        .zip(items)
                        .map(|((idx, attr), item)| {
                            let result = match (item.drv_path, item.error) {
                                (Some(drv), _) => Ok((drv, item.references)),
                                (None, Some(msg)) => Err(anyhow::anyhow!(msg)),
                                (None, None) => {
                                    Err(anyhow::anyhow!("eval worker returned empty result"))
                                }
                            };
                            (idx, (attr, result))
                        })
                        .collect();
                    anyhow::Ok((indexed, warnings))
                }
            })
            .collect();

        let mut indexed: Vec<(usize, ResolvedDerivation)> = Vec::new();
        let mut all_warnings: Vec<String> = Vec::new();
        while let Some(chunk_result) = tasks.next().await {
            match chunk_result {
                Ok((items, warnings)) => {
                    indexed.extend(items);
                    all_warnings.extend(warnings);
                }
                Err(e) => {
                    warn!(error = %e, "eval worker chunk failed");
                    return Err(e);
                }
            }
        }

        indexed.sort_by_key(|(idx, _)| *idx);
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
