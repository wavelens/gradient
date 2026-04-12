/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::path_utils::{nix_store_path, strip_store_prefix};
use super::{
    BuildExecutionResult, BuildExecutor, ExecutedBuildOutput, GenericDaemonClient,
    LocalDaemonClient,
};
use crate::executer::pool::{ConnectionPool, PathInfo, PooledConnectionGuard, convert_valid_path_info};
use crate::sources::decrypt_ssh_private_key;
use crate::sources::get_hash_from_path;
use crate::types::*;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use harmonia_protocol::build_result::BuildResultInner;
use harmonia_protocol::daemon_wire::types2::BuildMode;
use harmonia_store_remote::DaemonStore;
use harmonia_store_core::derivation::BasicDerivation;
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::{DaemonClientBuilder, HandshakeDaemonStore as _};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::Command,
    time::{self, Instant},
};
use tracing::{debug, error, info, instrument, warn};

#[instrument(skip(remote_store, _state), fields(build_id = %build.id, derivation_path = %derivation_path))]
pub async fn execute_build<R, W>(
    build: &MBuild,
    derivation_path: &str,
    derivation: &BasicDerivation,
    remote_store: &mut GenericDaemonClient<R, W>,
    _state: Arc<ServerState>,
) -> anyhow::Result<(MBuild, harmonia_protocol::build_result::BuildResult)>
where
    R: AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W: AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    info!("Executing build");

    let store_path = nix_store_path(derivation_path);
    let build = build.clone();

    let harmonia_path = StorePath::from_base_path(strip_store_prefix(&store_path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", store_path, e))?;

    let result = remote_store
        .build_derivation(&harmonia_path, derivation, BuildMode::Normal)
        .await
        .map_err(|e| anyhow::anyhow!("build_derivation failed: {}", e))?;

    Ok((build, result))
}

#[instrument(skip(from_store, to_store), fields(path_count = paths.len(), local_is_receiver))]
pub async fn copy_builds<R1, W1, R2, W2>(
    paths: Vec<String>,
    from_store: &mut GenericDaemonClient<R1, W1>,
    to_store: &mut GenericDaemonClient<R2, W2>,
    local_is_receiver: bool,
) -> Result<()>
where
    R1: AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W1: AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
    R2: AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W2: AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    for path in paths {
        info!(
            path = %path,
            destination = if local_is_receiver { "local" } else { "remote" },
            "Copying build"
        );

        let store_path = StorePath::from_base_path(strip_store_prefix(&nix_store_path(&path)))
            .map_err(|e| anyhow::anyhow!("Invalid store path: {}", e))?;

        let is_valid_dest = to_store
            .is_valid_path(&store_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check path validity in destination: {}", e))?;
        if is_valid_dest {
            continue;
        }

        let is_valid_src = from_store
            .is_valid_path(&store_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check path validity in source: {}", e))?;
        if !is_valid_src {
            anyhow::bail!("Path {} is not valid in source store", path);
        }

        let nar = from_store
            .nar_from_path(&store_path)
            .await
            .map_err(|e| anyhow::anyhow!("nar_from_path failed: {}", e))?;

        let path_info_opt = from_store
            .query_path_info(&store_path)
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?;

        let unkeyed = path_info_opt
            .ok_or_else(|| anyhow::anyhow!("Path info not found for {}", path))?;

        let valid_info = harmonia_protocol::valid_path_info::ValidPathInfo {
            path: store_path.clone(),
            info: unkeyed,
        };

        to_store
            .add_to_store_nar(&valid_info, nar, false, false)
            .await
            .map_err(|e| anyhow::anyhow!("add_to_store_nar failed: {}", e))?;

        let is_valid_after = to_store
            .is_valid_path(&store_path)
            .await
            .map_err(|e| anyhow::anyhow!("Post-copy validity check failed: {}", e))?;
        if !is_valid_after {
            anyhow::bail!("Path {} is not valid in destination store after copy", path);
        }
    }

    Ok(())
}

pub async fn get_missing_builds(pool: &ConnectionPool, paths: Vec<String>) -> Result<Vec<String>> {
    let mut output_paths: HashMap<String, String> = HashMap::new();
    let mut drv_paths: Vec<String> = Vec::new();

    for path in paths {
        if path.ends_with(".drv") {
            drv_paths.push(path);
        } else {
            output_paths.insert(path.clone(), nix_store_path(&path));
        }
    }

    if !drv_paths.is_empty() {
        let mut tasks: FuturesUnordered<_> = drv_paths
            .into_iter()
            .map(|path| async move {
                let mut guard = pool
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("acquire store for output map: {}", e))?;
                let full_path = nix_store_path(&path);
                let output_map = get_output_paths(full_path.clone(), guard.client())
                    .await
                    .with_context(|| format!("Failed to get output path for {}", full_path))?;
                anyhow::Ok((path, output_map))
            })
            .collect();

        while let Some(result) = tasks.next().await {
            let (path, output_map) = result?;
            for out_path in output_map.values() {
                output_paths.insert(path.clone(), out_path.clone());
            }
        }
    }

    let mut guard = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire store for valid paths: {}", e))?;

    let store_paths: harmonia_store_core::store_path::StorePathSet = output_paths
        .values()
        .filter_map(|p| StorePath::from_base_path(strip_store_prefix(p)).ok())
        .collect();

    let valid_paths = guard
        .client()
        .query_valid_paths(&store_paths, true)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query valid paths: {}", e))?;

    let valid_strings: std::collections::HashSet<String> = valid_paths
        .iter()
        .map(|p| format!("/nix/store/{}", p))
        .collect();

    let missing = output_paths
        .into_iter()
        .filter(|(_, v)| !valid_strings.contains(v))
        .map(|(k, _)| k)
        .collect();

    Ok(missing)
}

pub async fn get_output_paths<R, W>(
    path: String,
    store: &mut GenericDaemonClient<R, W>,
) -> Result<HashMap<String, String>>
where
    R: AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W: AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    let store_path = StorePath::from_base_path(strip_store_prefix(&path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", path, e))?;

    let output_map = store
        .query_derivation_output_map(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map failed: {}", e))?;

    Ok(output_map
        .into_iter()
        .filter_map(|(name, sp_opt)| sp_opt.map(|sp| (name.to_string(), format!("/nix/store/{}", sp))))
        .collect())
}

pub async fn get_local_store(
    organization: Option<MOrganization>,
) -> Result<LocalDaemonClient> {
    if organization.as_ref().is_none_or(|org| org.use_nix_store) {
        let client = DaemonClientBuilder::new()
            .build_unix("/nix/var/nix/daemon-socket/socket")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to daemon socket: {}", e))?
            .handshake()
            .await
            .map_err(|e| anyhow::anyhow!("Daemon handshake failed: {}", e))?;

        Ok(client)
    } else {
        let org = organization.ok_or_else(|| {
            anyhow::anyhow!("Organization should be Some when not using nix store")
        })?;
        let nix_store_dir = format!("/var/lib/gradient/store/{}", org.id);
        let mut child = Command::new("nix-store")
            .arg("--eval-store")
            .arg(nix_store_dir.clone())
            .arg("--serve")
            .env("NIX_STORE_DIR", nix_store_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn nix-store process")?;

        let _stdin = child.stdin.take();
        let _stdout = child.stdout.take();
        let _stderr = child.stderr.take();

        anyhow::bail!("Subprocess-based store not yet supported with harmonia — use use_nix_store=true");
    }
}

pub async fn get_pathinfo(
    path: String,
    guard: &mut PooledConnectionGuard,
) -> Result<Option<PathInfo>> {
    let store_path = StorePath::from_base_path(strip_store_prefix(&path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", path, e))?;

    let info = guard
        .client()
        .query_path_info(&store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?;

    Ok(info.map(|vi| convert_valid_path_info(&vi)))
}

#[derive(Debug, Clone)]
pub struct BuildOutputInfo {
    pub name: String,
    pub path: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
}

pub async fn get_build_outputs_from_derivation(
    derivation_path: String,
    guard: &mut PooledConnectionGuard,
) -> Result<Vec<BuildOutputInfo>> {
    let drv_store_path = StorePath::from_base_path(strip_store_prefix(&derivation_path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", derivation_path, e))?;

    let output_map = guard
        .client()
        .query_derivation_output_map(&drv_store_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map failed: {}", e))?;

    let mut outputs = Vec::new();

    for (output_name, output_store_path_opt) in &output_map {
        let Some(output_store_path) = output_store_path_opt else {
            continue;
        };

        let output_path_str = format!("/nix/store/{}", output_store_path);

        if let Some(_vi) = guard
            .client()
            .query_path_info(output_store_path)
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed: {}", e))?
        {
            let (hash, package) = get_hash_from_path(output_path_str.clone())
                .with_context(|| format!("Failed to parse path {}", output_path_str))?;

            outputs.push(BuildOutputInfo {
                name: output_name.to_string(),
                path: output_path_str,
                hash,
                package,
                ca: _vi.ca.as_ref().map(|ca| ca.to_string()),
            });
        }
    }

    Ok(outputs)
}

/// Production [`BuildExecutor`] backed by SSH + the Nix daemon protocol via harmonia.
#[derive(Debug, Default)]
pub struct SshBuildExecutor;

impl SshBuildExecutor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BuildExecutor for SshBuildExecutor {
    async fn execute(
        &self,
        state: Arc<ServerState>,
        server: MServer,
        organization: MOrganization,
        build: MBuild,
        derivation_path: String,
        derivation: BasicDerivation,
        dependencies: Vec<String>,
    ) -> Result<BuildExecutionResult> {
        let mut local_daemon = get_local_store(Some(organization.clone()))
            .await
            .context("Failed to acquire local nix store for build")?;

        let (private_key, public_key) = decrypt_ssh_private_key(
            state.cli.crypt_secret_file.clone(),
            organization.clone(),
            &state.cli.serve_url,
        )
        .context("Failed to decrypt SSH private key for server")?;

        let mut server_daemon_result = super::ssh::connect(
            server.clone(),
            None,
            public_key.clone(),
            private_key.clone(),
        )
        .await;

        for _ in 1..3 {
            if server_daemon_result.is_ok() {
                break;
            }
            time::sleep(Duration::from_secs(5)).await;
            server_daemon_result = super::ssh::connect(
                server.clone(),
                None,
                public_key.clone(),
                private_key.clone(),
            )
            .await;
        }

        let mut server_daemon =
            server_daemon_result.context("Failed to connect to server after retries")?;

        info!(server_id = %server.id, "Connected to build server");

        copy_builds(
            dependencies.clone(),
            &mut local_daemon,
            &mut server_daemon,
            false,
        )
        .await
        .context("Failed to copy build dependencies to server")?;

        let build_start = Instant::now();
        let (build, daemon_result) = execute_build(
            &build,
            &derivation_path,
            &derivation,
            &mut server_daemon,
            Arc::clone(&state),
        )
        .await
        .context("Failed to execute build on server")?;
        let elapsed = build_start.elapsed();

        match &daemon_result.inner {
            BuildResultInner::Failure(f) => {
                let error_msg = String::from_utf8_lossy(&f.error_msg).to_string();
                warn!(
                    build_id = %build.id,
                    error = %error_msg,
                    "Remote build reported failure"
                );
                return Ok(BuildExecutionResult {
                    error_msg,
                    outputs: vec![],
                    elapsed,
                });
            }
            BuildResultInner::Success(s) => {
                let copy_back: Vec<String> = s
                    .built_outputs
                    .values()
                    .map(|r| format!("/nix/store/{}", r.out_path))
                    .collect();

                copy_builds(
                    copy_back,
                    &mut server_daemon,
                    &mut local_daemon,
                    true,
                )
                .await
                .context("Failed to copy build outputs back to local store")?;

                let mut outputs = Vec::with_capacity(s.built_outputs.len());
                for (output_name, realisation) in &s.built_outputs {
                    let store_path_str = format!("/nix/store/{}", realisation.out_path);
                    let (hash, package) = match get_hash_from_path(store_path_str.clone()) {
                        Ok(hp) => hp,
                        Err(e) => {
                            error!(error = %e, path = %store_path_str, "Failed to parse output path");
                            continue;
                        }
                    };

                    let has_artefacts =
                        tokio::fs::metadata(format!("{}/nix-support/hydra-build-products", store_path_str))
                            .await
                            .is_ok();

                    let sp = StorePath::from_base_path(strip_store_prefix(&store_path_str)).ok();
                    let nar_size = if let Some(ref sp) = sp {
                        match local_daemon.query_path_info(sp).await {
                            Ok(Some(info)) => Some(info.nar_size as i64),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    debug!(name = %output_name, path = %store_path_str, "Recorded built output");
                    outputs.push(ExecutedBuildOutput {
                        name: output_name.to_string(),
                        store_path: store_path_str,
                        hash,
                        package,
                        nar_size,
                        has_artefacts,
                    });
                }

                Ok(BuildExecutionResult {
                    error_msg: String::new(),
                    outputs,
                    elapsed,
                })
            }
        }
    }
}
