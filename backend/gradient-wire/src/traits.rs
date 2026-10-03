/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{
    BuildMetrics, BuildOutput, CachedPath, DiscoveredDerivation, FailedPeer, GradientCapabilities,
    QueryMode,
};

#[async_trait]
pub trait WorkerStore: Send + Sync {
    async fn has_path(&self, store_path: &str) -> Result<bool>;

    async fn add_nar(&self, name: &str, nar: Vec<u8>) -> Result<String>;
}

#[async_trait]
pub trait DrvReader: Send + Sync {
    async fn read_drv(&self, store_path: &str) -> Result<Vec<u8>>;
}

#[async_trait]
pub trait JobReporter: Send + Sync {
    async fn query_cache(&mut self, paths: Vec<String>, mode: QueryMode)
    -> Result<Vec<CachedPath>>;

    async fn query_upstream(&mut self, path: String) -> Result<Option<CachedPath>>;

    async fn query_known_derivations(&self, drv_paths: Vec<String>) -> Result<Vec<String>>;
    async fn report_fetching(&mut self) -> Result<()>;
    async fn report_fetch_result(&mut self, flake_source: Option<String>) -> Result<()>;

    async fn report_input_update(
        &mut self,
        candidate_lock: String,
        bumped: Vec<crate::messages::BumpedInputWire>,
    ) -> Result<()> {
        let _ = (candidate_lock, bumped);
        Ok(())
    }

    async fn report_input_expansion(&mut self, matched: Vec<String>) -> Result<()> {
        let _ = matched;
        Ok(())
    }
    async fn report_evaluating_flake(&mut self) -> Result<()>;
    async fn report_evaluating_derivations(&mut self) -> Result<()>;
    async fn report_eval_result(
        &self,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()>;

    /// Every batch must be pushed before [`report_eval_result`](Self::report_eval_result). A build
    /// of the batch is then able to pull everything once the server can dispatch it. A failed
    /// upload is failing the evaluation instead of a later build.
    async fn push_paths(&self, paths: &[(String, Option<u64>)]) -> Result<()>;

    async fn report_building(&mut self, build_id: String) -> Result<()>;
    async fn report_build_output(
        &mut self,
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()>;
    async fn report_compressing(&mut self) -> Result<()>;
    async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()>;
    async fn send_eval_message(
        &mut self,
        level: crate::messages::EvalMessageLevel,
        source: &str,
        message: &str,
    ) -> Result<()>;

    fn eval_progress_sink(&self) -> std::sync::Arc<dyn EvalProgressSink>;
}

#[async_trait]
pub trait EvalProgressSink: Send + Sync {
    async fn report(&self, progress: crate::types::EvalProgress);
}

#[async_trait]
pub trait PeerIdentity: Send + Sync {
    fn peer_id(&self) -> String;

    async fn tokens_for(&self, peers: &[String]) -> Result<Vec<(String, String)>>;
}

#[async_trait]
pub trait CapabilitiesProvider: Send + Sync {
    async fn capabilities(&self) -> GradientCapabilities;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthOutcome {
    Accept {
        authorized_peers: Vec<String>,
        failed_peers: Vec<FailedPeer>,
    },
    Reject {
        code: u16,
        reason: String,
    },
}

#[async_trait]
pub trait PeerAuthority: Send + Sync {
    type Challenge: Send;

    async fn challenge(&self, claimed: &str) -> Result<(Self::Challenge, Vec<String>)>;

    async fn authorize(
        &self,
        claimed: &str,
        challenge: Self::Challenge,
        tokens: &[(String, String)],
    ) -> Result<AuthOutcome>;

    async fn negotiate(
        &self,
        claimed: &str,
        client: GradientCapabilities,
    ) -> Result<GradientCapabilities>;
}

#[async_trait]
pub trait DialerAuthority: Send + Sync {
    async fn admit(&self, worker_id: &str) -> Result<AuthOutcome>;

    async fn negotiate(
        &self,
        worker_id: &str,
        client: GradientCapabilities,
    ) -> Result<GradientCapabilities>;
}

#[async_trait]
pub trait DialerVerifier: Send + Sync {
    async fn verify(&self, worker_id: &str, tokens: &[(String, String)]) -> bool;
}
