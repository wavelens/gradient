/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Eval capability: flake fetch + evaluation dispatch and reporting traits.

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{DiscoveredDerivation, FlakeJob};

#[async_trait]
pub trait EvalServer: Send + Sync {
    /// Start evaluating the given flake job.
    async fn start_eval(&self, job_id: String, job: FlakeJob) -> Result<()>;

    /// Abort a previously-started eval by job id.
    async fn abort_eval(&self, job_id: String) -> Result<()>;
}

#[async_trait]
pub trait EvalClient: Send + Sync {
    /// Called when a peer reports completion of an eval job with discovered
    /// derivations and any warnings/errors.
    async fn on_eval_result(
        &self,
        peer_id: String,
        job_id: String,
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;

    #[async_trait]
    impl EvalServer for Noop {
        async fn start_eval(&self, _: String, _: FlakeJob) -> Result<()> {
            Ok(())
        }
        async fn abort_eval(&self, _: String) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl EvalClient for Noop {
        async fn on_eval_result(
            &self,
            _: String,
            _: String,
            _: Vec<DiscoveredDerivation>,
            _: Vec<String>,
            _: Vec<String>,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn noop_impls_drive() {
        let s: &dyn EvalServer = &Noop;
        s.start_eval(
            "j1".into(),
            FlakeJob {
                tasks: vec![],
                source: crate::messages::FlakeSource::Cached {
                    store_path: "/nix/store/x".into(),
                },
                wildcards: vec![],
                timeout_secs: None,
                input_overrides: vec![],
                input_update: None,
            },
        )
        .await
        .unwrap();
        s.abort_eval("j1".into()).await.unwrap();

        let c: &dyn EvalClient = &Noop;
        c.on_eval_result("p1".into(), "j1".into(), vec![], vec![], vec![])
            .await
            .unwrap();
    }
}
