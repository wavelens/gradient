/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use nix_daemon::nix::DaemonStore;
use nix_daemon::{self, BasicDerivation, BuildMode, BuildResult, PathInfo, Progress, Store};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
    time::Instant,
};
use tracing::{error, info, instrument};

use super::input;
use super::sources::get_hash_from_path;
use super::types::*;

pub async fn connect(
    server: MServer,
    store_path: Option<String>,
    public_key: String,
    private_key: String,
) -> anyhow::Result<NixStore> {
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

    Ok(DaemonStore::builder().init(BoxedIo::new(channel)).await?)
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

#[instrument(skip(remote_store, state), fields(build_id = %build.id, derivation_path = %build.derivation_path))]
pub async fn execute_build(
    build: &MBuild,
    derivation: BasicDerivation,
    remote_store: &mut NixStore,
    state: Arc<ServerState>,
) -> anyhow::Result<(MBuild, BuildResult)> {
    info!("Executing build");

    let derivation_path = get_builds_path(vec![&build]).first().unwrap().to_string();
    let build = build.clone();

    let mut prog = remote_store.build_derivation(derivation_path, derivation, BuildMode::Normal);

    const FLUSH_INTERVAL_MS: u128 = 1000;
    const FLUSH_THRESHOLD_BYTES: usize = 64 * 1024;

    let mut pending = String::new();
    let mut last_flush = Instant::now();

    while let Some(stderr) = prog.next().await? {
        let text = match &stderr {
            nix_daemon::Stderr::Next(s) => Some(s.clone()),
            nix_daemon::Stderr::Result(res)
                if res.kind == nix_daemon::StderrResultType::BuildLogLine
                    || res.kind == nix_daemon::StderrResultType::PostBuildLogLine =>
            {
                res.fields.first().and_then(|f| f.as_string()).cloned()
            }
            _ => None,
        };

        if let Some(text) = text
            && !text.is_empty()
        {
            pending.push_str(&text);
            if !text.ends_with('\n') {
                pending.push('\n');
            }

            let elapsed = last_flush.elapsed().as_millis();
            if elapsed >= FLUSH_INTERVAL_MS || pending.len() >= FLUSH_THRESHOLD_BYTES {
                state
                    .log_storage
                    .append(build.id, &pending)
                    .await
                    .context("Failed to append build log")?;
                pending.clear();
                last_flush = Instant::now();
            }
        }
    }

    // Flush any remaining buffered log
    if !pending.is_empty() {
        state
            .log_storage
            .append(build.id, &pending)
            .await
            .context("Failed to append build log")?;
    }

    match prog.result().await.map_err(|e| e.into()) {
        Ok(result) => Ok((build, result)),
        Err(e) => Err(e),
    }
}

#[instrument(skip(from_store, to_store), fields(path_count = paths.len(), local_is_receiver))]
pub async fn copy_builds<
    A: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    B: AsyncReadExt + AsyncWriteExt + Unpin + Send,
>(
    paths: Vec<String>,
    from_store: &mut DaemonStore<A>,
    to_store: &mut DaemonStore<B>,
    local_is_receiver: bool,
) -> Result<()> {
    for path in paths {
        info!(
            path = %path,
            destination = if local_is_receiver { "local" } else { "remote" },
            "Copying build"
        );

        if to_store
            .is_valid_path(path.clone())
            .result()
            .await
            .context("Failed to check path validity in destination store")?
        {
            continue;
        }

        if !from_store
            .is_valid_path(path.clone())
            .result()
            .await
            .context("Failed to check path validity in source store")?
        {
            anyhow::bail!("Path {} is not valid in source store", path);
        }

        let nar = from_store.nar_from_path(path.clone()).result().await?;
        let path_info = from_store
            .query_pathinfo(path.clone())
            .result()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Path info not found for {}", path))?;

        to_store
            .add_to_store_nar(path.clone(), path_info, nar)
            .result()
            .await?;

        if !to_store
            .is_valid_path(path.clone())
            .result()
            .await
            .context("Failed to check path validity in destination store")?
        {
            anyhow::bail!("Path {} is not valid in destination store", path);
        }
    }

    Ok(())
}

pub async fn get_missing_builds<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    paths: Vec<String>,
    store: &mut DaemonStore<A>,
) -> Result<Vec<String>> {
    let mut output_paths: HashMap<String, String> = HashMap::new();

    for path in paths {
        if path.ends_with(".drv") {
            let full_path = nix_store_path(&path);
            let output_map = get_output_paths(full_path.clone(), store)
                .await
                .with_context(|| format!("Failed to get output path for {}", full_path))?;

            // TODO: Handle multiple outputs properly
            for out_path in output_map.values() {
                output_paths.insert(path.clone(), out_path.clone());
            }
        } else {
            output_paths.insert(path.clone(), nix_store_path(&path));
        }
    }

    let valid_paths = store
        .query_valid_paths(output_paths.values().clone(), true)
        .result()
        .await
        .context("Failed to query valid paths")?;

    let missing = output_paths
        .into_iter()
        .filter(|(_, v)| !valid_paths.contains(v))
        .map(|(k, _)| k)
        .collect::<Vec<String>>();

    Ok(missing)
}

pub async fn get_output_paths<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    path: String,
    store: &mut DaemonStore<A>,
) -> Result<HashMap<String, String>> {
    let output_map = store
        .query_derivation_output_map(path.clone())
        .result()
        .await?;
    Ok(output_map)
}

pub async fn get_local_store(organization: Option<MOrganization>) -> Result<LocalNixStore> {
    if organization.as_ref().is_none_or(|org| org.use_nix_store) {
        let socket = UnixStream::connect("/nix/var/nix/daemon-socket/socket")
            .await
            .context("Failed to connect to Nix daemon socket")?;

        DaemonStore::builder()
            .init(BoxedIo::new(socket))
            .await
            .context("Failed to connect to local Nix daemon")
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

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdout"))?;
        let _stderr = child.stderr.take();

        DaemonStore::builder()
            .init(BoxedIo::new(tokio::io::join(stdout, stdin)))
            .await
            .context("Failed to initialize daemon store")
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

pub fn get_builds_path(builds: Vec<&MBuild>) -> Vec<String> {
    builds
        .iter()
        .map(|build| nix_store_path(&build.derivation_path))
        .collect()
}

pub async fn get_pathinfo<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    path: String,
    store: &mut DaemonStore<A>,
) -> Result<Option<PathInfo>> {
    store
        .query_pathinfo(path)
        .result()
        .await
        .context("Failed to query path info")
}

#[derive(Debug, Clone)]
pub struct BuildOutputInfo {
    pub name: String,
    pub path: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
}

pub async fn get_build_outputs_from_derivation<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    derivation_path: String,
    store: &mut DaemonStore<A>,
) -> Result<Vec<BuildOutputInfo>> {
    let output_map = store
        .query_derivation_output_map(derivation_path)
        .result()
        .await
        .context("Failed to query derivation output map")?;

    let mut outputs = Vec::new();

    for (output_name, output_path) in output_map {
        if let Some(path_info) = store
            .query_pathinfo(output_path.clone())
            .result()
            .await
            .context("Failed to query path info")?
        {
            let (hash, package) = get_hash_from_path(output_path.clone())
                .with_context(|| format!("Failed to parse path {}", output_path))?;

            outputs.push(BuildOutputInfo {
                name: output_name,
                path: output_path,
                hash,
                package,
                ca: path_info.ca,
            });
        }
    }

    Ok(outputs)
}
