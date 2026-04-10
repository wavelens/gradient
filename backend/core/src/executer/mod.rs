/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod nix_store;
pub mod path_utils;
pub mod pool;
pub mod ssh;

pub use self::pool::*;
pub use self::nix_store::{
    BuildOutputInfo, SshBuildExecutor, copy_builds, execute_build,
    get_build_outputs_from_derivation, get_local_store, get_missing_builds, get_output_paths,
    get_pathinfo,
};
pub use self::path_utils::{get_derivation_paths, nix_store_path, strip_nix_store_prefix};
pub use self::ssh::{connect, init_session};

use anyhow::Result;
use async_trait::async_trait;
use harmonia_store_core::derivation::BasicDerivation;
use crate::types::*;
use std::sync::Arc;
use std::time::Duration;

/// Re-export harmonia's BasicDerivation for use by builder/scheduler.
pub use harmonia_store_core::derivation::BasicDerivation as HarmoniaBasicDerivation;

/// A Nix daemon client over any transport. Generic over read/write halves.
pub type GenericDaemonClient<R, W> = harmonia_store_remote::DaemonClient<R, W>;

/// A Nix daemon client over a Unix socket (the local daemon).
pub type LocalDaemonClient =
    harmonia_store_remote::DaemonClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>;

/// One realised output of a build, populated by `BuildExecutor::execute`.
#[derive(Debug, Clone)]
pub struct ExecutedBuildOutput {
    /// Output name (`out`, `dev`, ...).
    pub name: String,
    /// Full `/nix/store/...` path of the realised output.
    pub store_path: String,
    /// `<hash>-<package>` portion of the store path.
    pub hash: String,
    pub package: String,
    /// NAR size as reported by the local store after copying back.
    pub nar_size: Option<i64>,
    /// `true` if `<output>/nix-support/hydra-build-products` exists.
    pub has_artefacts: bool,
}

/// End-to-end result of running one build on a remote server.
#[derive(Debug, Clone)]
pub struct BuildExecutionResult {
    /// Empty on success; non-empty when the daemon reported a build failure.
    pub error_msg: String,
    /// Realised outputs (empty on failure).
    pub outputs: Vec<ExecutedBuildOutput>,
    /// Wall-clock time spent inside `execute_build`.
    pub elapsed: Duration,
}

/// Executes builds on remote build servers via SSH-tunneled Nix daemon
/// connections. The trait abstraction lets tests substitute a deterministic
/// fake instead of touching real SSH/daemon infrastructure.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait BuildExecutor: Send + Sync + std::fmt::Debug + 'static {
    async fn execute(
        &self,
        state: Arc<ServerState>,
        server: MServer,
        organization: MOrganization,
        build: MBuild,
        derivation_path: String,
        derivation: BasicDerivation,
        dependencies: Vec<String>,
    ) -> Result<BuildExecutionResult>;
}
