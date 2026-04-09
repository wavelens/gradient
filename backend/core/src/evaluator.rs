/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Trait abstraction over the Nix derivation evaluator.
//!
//! Production impl lives in the `builder` crate (`WorkerPoolResolver`) and
//! drives a pool of long-lived eval-worker subprocesses, each hosting one
//! persistent embedded Nix C API evaluator. Tests in any crate can substitute
//! the in-memory `FakeDerivationResolver` from `test-support`.

use crate::derivation::Derivation;
use anyhow::Result;
use async_trait::async_trait;
use entity::server::Architecture;

/// Result of resolving one flake attribute path: `(attr_path, Result<(drv_path, references)>)`.
pub type ResolvedDerivation = (String, Result<(String, Vec<String>)>);

/// Evaluates flake-based Nix derivations. All methods are async; production
/// impls run their work inside `tokio::task::spawn_blocking` to keep the
/// embedded Nix C API off Tokio worker threads (Boehm GC vs. signal-blocked
/// workers — see `builder::evaluator::nix_eval`).
#[async_trait]
pub trait DerivationResolver: Send + Sync + std::fmt::Debug + 'static {
    /// Discover all attribute paths matching `wildcards` in the given flake.
    /// Returns `(attr_paths, warnings)`.
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)>;

    /// Resolve a batch of attribute paths into `(drv_path, references)` tuples.
    /// The result preserves the input order of `attrs`.
    /// Returns `(resolved, warnings)`.
    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedDerivation>, Vec<String>)>;

    /// Read and parse a `.drv` file at `drv_path`.
    async fn get_derivation(&self, drv_path: String) -> Result<Derivation>;

    /// Returns `(system_architecture, required_features)` for the derivation
    /// at `drv_path`. For non-`.drv` paths returns `(BUILTIN, [])`.
    async fn get_features(&self, drv_path: String) -> Result<(Architecture, Vec<String>)>;
}
