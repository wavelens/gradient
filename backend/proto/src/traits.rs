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

use crate::messages::{BuildOutput, CachedPath, DiscoveredDerivation, FetchedInput};

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
    /// Query the server's cache for which paths are available.
    ///
    /// Returns all available paths as `CachedPath`. Local Gradient cache entries
    /// have `url: None`; upstream external cache entries have `url: Some(abs_url)`.
    async fn query_cache(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>>;
    async fn report_fetching(&mut self) -> Result<()>;
    async fn report_fetch_result(&mut self, fetched_paths: Vec<FetchedInput>) -> Result<()>;
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
