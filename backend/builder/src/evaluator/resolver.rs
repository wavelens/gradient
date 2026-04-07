/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Production [`DerivationResolver`] backed by the embedded Nix C API.
//!
//! All evaluator-touching work runs inside `tokio::task::spawn_blocking` to
//! keep the Boehm GC away from signal-blocked Tokio worker threads. The batch
//! `resolve_derivation_paths` method also fans out across worker threads with
//! `std::thread::scope`, sharing one `NixEvaluator` instance.

use anyhow::{Context, Result};
use async_trait::async_trait;
use entity::server::Architecture;
use gradient_core::derivation::{Derivation, parse_drv};
use gradient_core::evaluator::{DerivationResolver, ResolvedDerivation};
use std::sync::Arc;

use super::flake::discover_derivations;
use super::nix_commands::get_derivation_path;
use super::nix_eval::NixEvaluator;

/// Strips the `/nix/store/` prefix and returns just the hash-name component.
fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

#[derive(Debug, Default)]
pub struct NixCApiResolver;

impl NixCApiResolver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DerivationResolver for NixCApiResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<Vec<String>> {
        // Expand a bare `*` into `*.*` and `*.*.*` so that derivations at depth 2
        // (e.g. formatter.x86_64-linux) and depth 3 (e.g. packages.x86_64-linux.hello)
        // are both discovered.
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

        tokio::task::spawn_blocking(move || discover_derivations(&repository, &expanded))
            .await
            .map_err(|e| anyhow::anyhow!("evaluator task panicked: {}", e))?
    }

    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<Vec<ResolvedDerivation>> {
        let resolved = tokio::task::spawn_blocking(move || -> Vec<ResolvedDerivation> {
            let evaluator = match NixEvaluator::new() {
                Ok(e) => Arc::new(e),
                Err(e) => {
                    let err_str = e.to_string();
                    return attrs
                        .into_iter()
                        .map(|d| {
                            (
                                d,
                                Err(anyhow::anyhow!(
                                    "failed to initialize nix evaluator: {}",
                                    err_str
                                )),
                            )
                        })
                        .collect();
                }
            };

            let n_workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(attrs.len().max(1));

            // Round-robin partition keeps chunk sizes balanced even when
            // per-derivation evaluation cost varies wildly.
            let mut chunks: Vec<Vec<(usize, String)>> =
                (0..n_workers).map(|_| Vec::new()).collect();
            for (idx, d) in attrs.into_iter().enumerate() {
                chunks[idx % n_workers].push((idx, d));
            }

            let mut indexed: Vec<(usize, ResolvedDerivation)> = std::thread::scope(|scope| {
                let handles: Vec<_> = chunks
                    .into_iter()
                    .map(|chunk| {
                        let evaluator = Arc::clone(&evaluator);
                        let nix_repo = repository.clone();
                        scope.spawn(move || {
                            chunk
                                .into_iter()
                                .map(|(idx, derivation_string)| {
                                    let result = get_derivation_path(
                                        &evaluator,
                                        &nix_repo,
                                        &derivation_string,
                                    );
                                    (idx, (derivation_string, result))
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();

                handles
                    .into_iter()
                    .flat_map(|h| h.join().expect("derivation resolver worker panicked"))
                    .collect()
            });

            indexed.sort_by_key(|(idx, _)| *idx);
            indexed.into_iter().map(|(_, r)| r).collect()
        })
        .await
        .map_err(|e| anyhow::anyhow!("derivation resolver task panicked: {}", e))?;

        Ok(resolved)
    }

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation> {
        let full_path = nix_store_path(&drv_path);
        let bytes = tokio::fs::read(&full_path)
            .await
            .with_context(|| format!("Failed to read derivation file: {}", full_path))?;

        parse_drv(&bytes).with_context(|| format!("Failed to parse derivation {}", drv_path))
    }

    async fn get_features(&self, drv_path: String) -> Result<(Architecture, Vec<String>)> {
        if !drv_path.ends_with(".drv") {
            return Ok((Architecture::BUILTIN, vec![]));
        }

        let drv = self.get_derivation(drv_path.clone()).await?;
        let features = drv.required_system_features();
        let system: Architecture = drv
            .system
            .as_str()
            .try_into()
            .map_err(|e| anyhow::anyhow!("{} has invalid system architecture: {:?}", drv_path, e))?;

        Ok((system, features))
    }
}
