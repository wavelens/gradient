/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use harmonia_protocol::daemon_wire::types2::BuildMode;
use harmonia_protocol::types::DaemonStore;
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

use super::pool::{ConnectionPool, PathInfo, PooledConnectionGuard, convert_valid_path_info};

use super::input;
use super::sources::{decrypt_ssh_private_key, get_hash_from_path};
use super::types::*;

/// A Nix daemon client over any transport. Generic over read/write halves.
pub type GenericDaemonClient<R, W> = harmonia_store_remote::DaemonClient<R, W>;

/// A Nix daemon client over a Unix socket (the local daemon).
pub type LocalDaemonClient =
    harmonia_store_remote::DaemonClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>;

pub async fn connect(
    server: MServer,
    store_path: Option<String>,
    public_key: String,
    private_key: String,
) -> anyhow::Result<GenericDaemonClient<tokio::io::ReadHalf<BoxedIo>, tokio::io::WriteHalf<BoxedIo>>> {
    let server_addr = input::url_to_addr(server.host.as_str(), server.port)?;
    let mut session = AsyncSession::<TokioTcpStream>::connect(server_addr, None).await?;

    init_session(
        &mut session,
        server.username.as_str(),
        public_key,
        private_key,
    )
    .await?;

    let mut channel = session.channel_session().await?;

    let command = if let Some(path) = store_path {
        format!("nix-daemon --stdio --option store {}", path)
    } else {
        "nix-daemon --stdio".to_string()
    };

    channel.exec(command.as_str()).await?;

    let io = BoxedIo::new(channel);
    let (reader, writer) = tokio::io::split(io);

    let client = DaemonClientBuilder::new()
        .connect(reader, writer)
        .await
        .map_err(|e| anyhow::anyhow!("Daemon handshake failed: {}", e))?;

    Ok(client)
}

pub async fn init_session(
    session: &mut AsyncSession<TokioTcpStream>,
    username: &str,
    public_key: String,
    private_key: String,
) -> anyhow::Result<()> {
    session.handshake().await.map_err(|err| {
        error!(error = ?err, "SSH handshake failed");
        err
    })?;

    session
        .userauth_pubkey_memory(
            username,
            Some(public_key.as_str()),
            private_key.as_str(),
            None,
        )
        .await?;
    assert!(session.authenticated());

    Ok(())
}

#[instrument(skip(remote_store, state), fields(build_id = %build.id, derivation_path = %derivation_path))]
pub async fn execute_build<R, W>(
    build: &MBuild,
    derivation_path: &str,
    derivation: nix_daemon::BasicDerivation,
    remote_store: &mut GenericDaemonClient<R, W>,
    state: Arc<ServerState>,
) -> anyhow::Result<(MBuild, nix_daemon::BuildResult)>
where
    R: AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W: AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    info!("Executing build");

    let store_path = nix_store_path(derivation_path);
    let build = build.clone();

    // Convert nix_daemon::BasicDerivation to harmonia's BasicDerivation
    let harmonia_drv = convert_basic_derivation(&derivation)?;
    let harmonia_path = StorePath::from_base_path(strip_store_prefix(&store_path))
        .map_err(|e| anyhow::anyhow!("Invalid store path {}: {}", store_path, e))?;

    // For now, just await the result directly (no log streaming).
    // TODO: Use ResultLog's Stream impl to stream build logs in real time.
    let harmonia_result = remote_store
        .build_derivation(&harmonia_path, &harmonia_drv, BuildMode::Normal)
        .await
        .map_err(|e| anyhow::anyhow!("build_derivation failed: {}", e))?;

    // Convert harmonia BuildResult back to nix_daemon::BuildResult for now
    // TODO: Remove this conversion once all consumers use harmonia types
    let result = convert_build_result_back(harmonia_result);

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

        // Construct a ValidPathInfo (keyed) from UnkeyedValidPathInfo
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
    // Separate plain store paths (no daemon call needed) from .drv paths.
    let mut output_paths: HashMap<String, String> = HashMap::new();
    let mut drv_paths: Vec<String> = Vec::new();

    for path in paths {
        if path.ends_with(".drv") {
            drv_paths.push(path);
        } else {
            output_paths.insert(path.clone(), nix_store_path(&path));
        }
    }

    // Resolve all .drv → output path mappings concurrently, one connection each.
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

    // Single batched validity check for all collected output paths.
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

        // TODO: This returns a different type than LocalDaemonClient.
        // The subprocess store path feature needs rework.
        anyhow::bail!("Subprocess-based store not yet supported with harmonia — use use_nix_store=true");
    }
}

/// Returns the full `/nix/store/` path for a derivation hash-name stored without prefix.
pub fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

/// Strips the `/nix/store/` prefix from a path, returning just the hash-name component.
pub fn strip_nix_store_prefix(path: &str) -> String {
    path.strip_prefix("/nix/store/").unwrap_or(path).to_string()
}

/// Strips the `/nix/store/` prefix, returning a `&str` (no allocation).
fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}

pub fn get_derivation_paths(derivations: &[MDerivation]) -> Vec<String> {
    derivations
        .iter()
        .map(|d| nix_store_path(&d.derivation_path))
        .collect()
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
        derivation: nix_daemon::BasicDerivation,
        dependencies: Vec<String>,
    ) -> Result<BuildExecutionResult>;
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
        derivation: nix_daemon::BasicDerivation,
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

        let mut server_daemon_result = connect(
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
            server_daemon_result = connect(
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
            derivation,
            &mut server_daemon,
            Arc::clone(&state),
        )
        .await
        .context("Failed to execute build on server")?;
        let elapsed = build_start.elapsed();

        if !daemon_result.error_msg.is_empty() {
            warn!(
                build_id = %build.id,
                error = %daemon_result.error_msg,
                "Remote build reported failure"
            );
            return Ok(BuildExecutionResult {
                error_msg: daemon_result.error_msg,
                outputs: vec![],
                elapsed,
            });
        }

        let copy_back: Vec<String> = daemon_result
            .built_outputs
            .values()
            .map(|r| format!("/nix/store/{}", r.out_path))
            .collect();

        copy_builds(
            copy_back.clone(),
            &mut server_daemon,
            &mut local_daemon,
            true,
        )
        .await
        .context("Failed to copy build outputs back to local store")?;

        let mut outputs = Vec::with_capacity(daemon_result.built_outputs.len());
        for (name, realisation) in daemon_result.built_outputs {
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

            debug!(name = %name, path = %store_path_str, "Recorded built output");
            outputs.push(ExecutedBuildOutput {
                name,
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

// ---------------------------------------------------------------------------
// Conversion helpers (temporary — remove once nix_daemon types are fully gone)
// ---------------------------------------------------------------------------

use harmonia_store_core::derivation::{
    BasicDerivation as HarmoniaBasicDerivation,
    DerivationOutput as HarmoniaDerivationOutput,
};

/// Convert nix_daemon::BasicDerivation to harmonia's BasicDerivation.
fn convert_basic_derivation(
    drv: &nix_daemon::BasicDerivation,
) -> Result<HarmoniaBasicDerivation> {
    use harmonia_store_core::ByteString;
    use harmonia_store_core::derivation::DerivationOutputs;
    use harmonia_store_core::store_path::{StorePathName, StorePathSet};

    let mut outputs = DerivationOutputs::new();
    for (name, output) in &drv.outputs {
        let path_str = output.path.as_deref()
            .ok_or_else(|| anyhow::anyhow!("Output {} has no path", name))?;
        let sp = StorePath::from_base_path(strip_store_prefix(path_str))
            .map_err(|e| anyhow::anyhow!("Invalid output path {}: {}", path_str, e))?;
        outputs.insert(
            name.parse().map_err(|e| anyhow::anyhow!("Invalid output name {}: {}", name, e))?,
            HarmoniaDerivationOutput::InputAddressed(sp),
        );
    }

    let mut input_srcs = StorePathSet::new();
    for src in &drv.input_srcs {
        let sp = StorePath::from_base_path(strip_store_prefix(src))
            .map_err(|e| anyhow::anyhow!("Invalid input src {}: {}", src, e))?;
        input_srcs.insert(sp);
    }

    let name: StorePathName = "unknown".parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse derivation name: {}", e))?;

    Ok(HarmoniaBasicDerivation {
        name,
        outputs,
        inputs: input_srcs,
        platform: ByteString::from(drv.platform.clone()),
        builder: ByteString::from(drv.builder.clone()),
        args: drv.args.iter().map(|a| ByteString::from(a.clone())).collect(),
        env: drv.env.iter().map(|(k, v)| (ByteString::from(k.clone()), ByteString::from(v.clone()))).collect(),
        structured_attrs: None,
    })
}

/// Convert harmonia BuildResult back to nix_daemon::BuildResult.
fn convert_build_result_back(
    result: harmonia_protocol::daemon_wire::types2::BuildResult,
) -> nix_daemon::BuildResult {
    use harmonia_protocol::build_result::BuildResultInner;

    match result.inner {
        BuildResultInner::Success(s) => nix_daemon::BuildResult {
            status: nix_daemon::BuildResultStatus::Built,
            error_msg: String::new(),
            times_built: result.times_built as u64,
            is_non_deterministic: false,
            start_time: chrono::DateTime::from_timestamp(result.start_time, 0)
                .unwrap_or_default(),
            stop_time: chrono::DateTime::from_timestamp(result.stop_time, 0)
                .unwrap_or_default(),
            cpu_user: None,
            cpu_system: None,
            built_outputs: s
                .built_outputs
                .into_iter()
                .map(|(output_name, realisation)| {
                    let name = output_name.to_string();
                    (
                        name.clone(),
                        nix_daemon::Realisation {
                            id: realisation.id.to_string(),
                            out_path: realisation.out_path.to_string(),
                            signatures: realisation.signatures.iter().map(|s| s.to_string()).collect(),
                            dependent_realisations: realisation
                                .dependent_realisations
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.to_string()))
                                .collect(),
                        },
                    )
                })
                .collect(),
        },
        BuildResultInner::Failure(f) => nix_daemon::BuildResult {
            status: nix_daemon::BuildResultStatus::PermanentFailure,
            error_msg: String::from_utf8_lossy(&f.error_msg).to_string(),
            times_built: result.times_built as u64,
            is_non_deterministic: f.is_non_deterministic,
            start_time: chrono::DateTime::from_timestamp(result.start_time, 0)
                .unwrap_or_default(),
            stop_time: chrono::DateTime::from_timestamp(result.stop_time, 0)
                .unwrap_or_default(),
            cpu_user: None,
            cpu_system: None,
            built_outputs: HashMap::new(),
        },
    }
}
