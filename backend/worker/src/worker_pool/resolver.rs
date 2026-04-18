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
use std::sync::Arc;
use tracing::warn;

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

/// `DerivationResolver` impl that drives an [`EvalWorkerPool`].
///
/// `list_flake_derivations` is dispatched to a single worker (the wildcard
/// `*` is recursive in eval.nix, so one call discovers all depths).
/// `resolve_derivation_paths` splits attrs across the pool for parallelism.
/// `get_derivation` and `get_features` parse `.drv` files directly from disk.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_store_path_absolute_unchanged() {
        let path = "/nix/store/hash-name";
        assert_eq!(nix_store_path(path), path);
    }

    #[test]
    fn nix_store_path_bare_prefixed() {
        assert_eq!(nix_store_path("hash-name"), "/nix/store/hash-name");
    }
}
