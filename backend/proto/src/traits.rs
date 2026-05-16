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

use crate::messages::{BuildOutput, CachedPath, DiscoveredDerivation, GradientCapabilities, QueryMode};

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
    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()>;
}

// ── Role-neutral peer primitives ─────────────────────────────────────────────

/// Supplies the peer's identity and the plaintext tokens used to authenticate
/// against the peers the server lists in `AuthChallenge`.
///
/// Production impls:
/// - `worker::config::WorkerConfig` — static `(peer_id, plaintext_token)` pairs from config.
/// - `proxy-core::upstream::ProxyUpstreamIdentity` — proxy's own worker token issued by gradient-server.
#[async_trait]
pub trait PeerIdentity: Send + Sync {
    /// Stable peer id advertised in `InitConnection.id`.
    fn peer_id(&self) -> String;

    /// Given the list of peers the server is asking us to prove control of,
    /// return `(peer_id, plaintext_token)` pairs for the subset we hold tokens
    /// for. Pairs for unknown peers are simply omitted; the server's
    /// `validate_tokens` will then list them in `failed_peers`.
    async fn tokens_for(&self, peers: &[String]) -> Result<Vec<(String, String)>>;
}

/// Supplies the `GradientCapabilities` advertised at handshake.
///
/// Production impls:
/// - `worker::config::StaticCapabilities` — read once from config.
/// - `proxy-core::pool::AggregatedCapabilities` — live aggregate over the
///   connected backend pool, recomputed on join/leave.
#[async_trait]
pub trait CapabilitiesProvider: Send + Sync {
    /// Capabilities to send in `InitConnection.capabilities` / `InitAck.capabilities`.
    async fn capabilities(&self) -> GradientCapabilities;
}

#[cfg(test)]
mod role_trait_tests {
    use super::*;

    struct FakePeer {
        id: String,
    }

    #[async_trait]
    impl PeerIdentity for FakePeer {
        fn peer_id(&self) -> String {
            self.id.clone()
        }
        async fn tokens_for(&self, peers: &[String]) -> Result<Vec<(String, String)>> {
            Ok(peers
                .iter()
                .map(|p| (p.clone(), format!("{p}-tok")))
                .collect())
        }
    }

    struct FakeCaps;

    #[async_trait]
    impl CapabilitiesProvider for FakeCaps {
        async fn capabilities(&self) -> GradientCapabilities {
            GradientCapabilities {
                build: true,
                ..Default::default()
            }
        }
    }

    #[tokio::test]
    async fn peer_identity_round_trip() {
        let p = FakePeer { id: "abc".into() };
        assert_eq!(p.peer_id(), "abc");
        let toks = p.tokens_for(&["abc".into()]).await.unwrap();
        assert_eq!(toks, vec![("abc".into(), "abc-tok".into())]);
    }

    #[tokio::test]
    async fn capabilities_provider_round_trip() {
        let c = FakeCaps;
        let caps = c.capabilities().await;
        assert!(caps.build);
        assert!(!caps.eval);
    }
}
