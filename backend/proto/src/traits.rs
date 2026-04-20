/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Trait abstractions for worker testability.
//!
//! Production code uses concrete implementations backed by the local nix-daemon
//! and proto WebSocket connection. Tests inject fakes from `test-support`.

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{BuildOutput, CachedPath, DiscoveredDerivation, QueryMode};

// ── Store access ─────────────────────────────────────────────────────────────

/// Abstraction over the local Nix store for path queries.
///
/// Production: `worker::store::LocalNixStore`
/// Test: `test_support::fakes::worker_store::FakeWorkerStore`
#[async_trait]
pub trait WorkerStore: Send + Sync {
    /// Check whether a store path is present in the local store.
    async fn has_path(&self, store_path: &str) -> Result<bool>;
}

// ── Derivation file reader ───────────────────────────────────────────────────

/// Abstraction over reading raw `.drv` file bytes from the store.
///
/// Production: `FsDrvReader` reads from `/nix/store/`.
/// Test: `test_support::fakes::drv_reader::FakeDrvReader` serves from memory.
#[async_trait]
pub trait DrvReader: Send + Sync {
    /// Read the raw bytes of a `.drv` file given its store path.
    ///
    /// `store_path` may be a bare hash-name or a full `/nix/store/...` path.
    async fn read_drv(&self, store_path: &str) -> Result<Vec<u8>>;
}

// ── Job status reporting ─────────────────────────────────────────────────────

/// Abstraction over reporting job progress back to the server.
///
/// Production: `worker::job::JobUpdater`
/// Test: `test_support::fakes::job_reporter::RecordingJobReporter`
#[async_trait]
pub trait JobReporter: Send {
    /// Query the server's cache for path availability and optional transfer URLs.
    ///
    /// `mode` controls what is returned:
    /// - [`QueryMode::Normal`] — only paths already in the cache (`cached: true`, no URLs).
    /// - [`QueryMode::Pull`]   — cached paths with presigned S3 GET URLs where available.
    /// - [`QueryMode::Push`]   — all paths; uncached ones include presigned S3 PUT URLs.
    async fn query_cache(&mut self, paths: Vec<String>, mode: QueryMode)
    -> Result<Vec<CachedPath>>;

    /// Query the server for which of the given `.drv` paths are already in its
    /// derivation table for the owning org.
    ///
    /// Returns the subset of `drv_paths` that the server already knows about.
    /// The BFS closure walker uses this to skip re-traversing subtrees of
    /// derivations that were fully recorded in a previous evaluation.
    async fn query_known_derivations(&mut self, drv_paths: Vec<String>) -> Result<Vec<String>>;
    async fn report_fetching(&mut self) -> Result<()>;
    async fn report_fetch_result(&mut self, flake_source: Option<String>) -> Result<()>;
    async fn report_evaluating_flake(&mut self) -> Result<()>;
    async fn report_evaluating_derivations(&mut self) -> Result<()>;
    async fn report_eval_result(
        &mut self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()>;
    async fn report_building(&mut self, build_id: String) -> Result<()>;
    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()>;
    async fn report_compressing(&mut self) -> Result<()>;
    async fn report_signing(&mut self) -> Result<()>;
    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()>;
}
