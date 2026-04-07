/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::executer::{BuildExecutionResult, BuildExecutor, ExecutedBuildOutput};
use gradient_core::types::{MBuild, MOrganization, MServer, ServerState};
use nix_daemon::BasicDerivation;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// One recorded `BuildExecutor::execute` invocation.
#[derive(Debug, Clone)]
pub struct RecordedBuildExecution {
    pub server_id: Uuid,
    pub organization_id: Uuid,
    pub build_id: Uuid,
    pub dependencies: Vec<String>,
}

/// In-memory `BuildExecutor` for unit tests.
///
/// By default `execute` records the call and returns a successful empty
/// `BuildExecutionResult`. Use `with_result` / `with_outputs` to script the
/// next response. Inspect `recorded()` to assert call ordering.
#[derive(Debug, Default)]
pub struct FakeBuildExecutor {
    recorded: Mutex<Vec<RecordedBuildExecution>>,
    /// Optional pre-set responses, popped FIFO. When empty, `execute` returns
    /// a default success.
    next_results: Mutex<Vec<Result<BuildExecutionResult, String>>>,
}

impl FakeBuildExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a successful result with the given outputs (returned on the next
    /// `execute` call).
    pub fn with_outputs(self, outputs: Vec<ExecutedBuildOutput>) -> Self {
        self.next_results
            .lock()
            .unwrap()
            .push(Ok(BuildExecutionResult {
                error_msg: String::new(),
                outputs,
                elapsed: Duration::ZERO,
            }));
        self
    }

    /// Queue a build-failure result (`Ok` with non-empty `error_msg`).
    pub fn with_failure(self, msg: impl Into<String>) -> Self {
        self.next_results
            .lock()
            .unwrap()
            .push(Ok(BuildExecutionResult {
                error_msg: msg.into(),
                outputs: vec![],
                elapsed: Duration::ZERO,
            }));
        self
    }

    /// Queue an infrastructure error (`Err`).
    pub fn with_error(self, msg: impl Into<String>) -> Self {
        self.next_results.lock().unwrap().push(Err(msg.into()));
        self
    }

    pub fn recorded(&self) -> Vec<RecordedBuildExecution> {
        self.recorded.lock().unwrap().clone()
    }
}

#[async_trait]
impl BuildExecutor for FakeBuildExecutor {
    async fn execute(
        &self,
        _state: Arc<ServerState>,
        server: MServer,
        organization: MOrganization,
        build: MBuild,
        _derivation: BasicDerivation,
        dependencies: Vec<String>,
    ) -> Result<BuildExecutionResult> {
        self.recorded.lock().unwrap().push(RecordedBuildExecution {
            server_id: server.id,
            organization_id: organization.id,
            build_id: build.id,
            dependencies,
        });

        let next = self.next_results.lock().unwrap().pop();
        match next {
            Some(Ok(r)) => Ok(r),
            Some(Err(e)) => Err(anyhow::anyhow!(e)),
            None => Ok(BuildExecutionResult {
                error_msg: String::new(),
                outputs: vec![],
                elapsed: Duration::ZERO,
            }),
        }
    }
}
