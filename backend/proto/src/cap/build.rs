/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build capability: dispatch and reporting traits.

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{BuildJob, BuildOutput};

/// Server side of the `build` capability: receives build jobs and reports
/// build progress / results back upstream.
///
/// Implemented by `gradient-worker` (`worker::executor::BuildExecutor`) and
/// by `gradient-proxy` (forwards to the chosen backend).
#[async_trait]
pub trait BuildServer: Send + Sync {
    /// Start executing the given build job.
    async fn start_build(&self, job_id: String, job: BuildJob) -> Result<()>;

    /// Abort a previously-started build by job id. No-op if unknown.
    async fn abort_build(&self, job_id: String) -> Result<()>;
}

/// Client side of the `build` capability: dispatches build jobs to peers
/// and consumes build outputs.
///
/// Implemented by `gradient-server` (`web::scheduler_glue::BuildDispatcher`)
/// and by `gradient-proxy` (forwards downstream).
#[async_trait]
pub trait BuildClient: Send + Sync {
    /// Called when a peer reports completion of a build with its outputs.
    async fn on_build_output(
        &self,
        peer_id: String,
        job_id: String,
        outputs: Vec<BuildOutput>,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    #[async_trait]
    impl BuildServer for Noop {
        async fn start_build(&self, _: String, _: BuildJob) -> Result<()> {
            Ok(())
        }
        async fn abort_build(&self, _: String) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl BuildClient for Noop {
        async fn on_build_output(
            &self,
            _: String,
            _: String,
            _: Vec<BuildOutput>,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn noop_impls_compile_and_drive() {
        let s: &dyn BuildServer = &Noop;
        s.start_build("j1".into(), BuildJob { builds: vec![] })
            .await
            .unwrap();
        s.abort_build("j1".into()).await.unwrap();

        let c: &dyn BuildClient = &Noop;
        c.on_build_output("p1".into(), "j1".into(), vec![]).await.unwrap();
    }
}
