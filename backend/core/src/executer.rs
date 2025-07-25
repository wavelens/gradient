/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use futures::Stream;
use nix_daemon::nix::DaemonStore;
use nix_daemon::{self, BuildMode, BuildResult, PathInfo, Progress, Store};
use sea_orm::ActiveModelTrait;
use sea_orm::ActiveValue::Set;
use serde::Serialize;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
};
use tracing::{error, info, instrument};
use uuid::Uuid;

use super::input;
use super::sources::get_hash_from_path;
use super::types::*;

#[derive(Debug, Clone, Serialize)]
pub struct BuildLogStreamResponse {
    pub build_id: Uuid,
    pub log: String,
}

pub async fn connect(
    server: MServer,
    store_path: Option<String>,
    public_key: String,
    private_key: String,
) -> Result<NixStore, Box<dyn std::error::Error + Send + Sync>> {
    let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();
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

    Ok(DaemonStore::builder().init(channel).await?)
}

pub async fn init_session(
    session: &mut AsyncSession<TokioTcpStream>,
    username: &str,
    public_key: String,
    private_key: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    session.handshake().await.unwrap_or_else(|err| {
        error!(error = ?err, "SSH handshake failed");
    });

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
    remote_store: &mut NixStore,
    state: Arc<ServerState>,
) -> Result<(MBuild, HashMap<String, BuildResult>), Box<dyn std::error::Error + Send + Sync>> {
    info!("Executing build");

    let paths = get_builds_path(vec![&build]);
    let mut build = build.clone();

    for path in paths.clone() {
        remote_store.ensure_path(path).result().await?;
    }

    let paths = paths
        .iter()
        .map(|p| format!("{}!*", p).to_string())
        .collect::<Vec<String>>();

    let mut prog = remote_store.build_paths_with_results(paths, BuildMode::Normal);

    while let Some(stderr) = prog.next().await? {
        if let nix_daemon::Stderr::Result(res) = stderr {
            // if res.kind != nix_daemon::StderrResultType::BuildLogLine {
            //     continue;
            // }

            let log = res
                .fields
                .iter()
                .map(|l| l.as_string().unwrap_or(&"".to_string()).clone())
                .filter(|l| !l.replace("/n", "").is_empty())
                .collect::<Vec<String>>()
                .join("");

            let full_log = format!("{}\n{}", build.log.as_ref().unwrap_or(&"".to_string()), log)
                .trim()
                .to_string();

            let mut abuild: ABuild = build.clone().into();
            abuild.log = Set(Some(full_log));
            build = abuild.update(&state.db).await.unwrap();
        }
    }

    match prog.result().await.map_err(|e| e.into()) {
        Ok(results) => Ok((build, results)),
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for path in paths {
        info!(
            path = %path,
            destination = if local_is_receiver { "local" } else { "remote" },
            "Copying build"
        );

        if to_store.is_valid_path(path.clone()).result().await.unwrap() {
            continue;
        }

        if !from_store
            .is_valid_path(path.clone())
            .result()
            .await
            .unwrap_or(false)
        {
            return Err(format!("Path {} is not valid in source store", path).into());
        }

        let nar = from_store.nar_from_path(path.clone()).result().await?;
        let path_info = from_store
            .query_pathinfo(path.clone())
            .result()
            .await?
            .unwrap();
        to_store
            .add_to_store_nar(path.clone(), path_info, nar)
            .result()
            .await
            .unwrap();

        if !to_store.is_valid_path(path.clone()).result().await.unwrap() {
            return Err(format!("Path {} is not valid in destination store", path).into());
        }
    }

    Ok(())
}

pub async fn get_missing_builds<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    paths: Vec<String>,
    store: &mut DaemonStore<A>,
) -> Result<Vec<String>, String> {
    let mut output_paths: HashMap<String, String> = HashMap::new();

    for path in paths {
        if path.ends_with(".drv") {
            let output_map = store
                .query_derivation_output_map(path.clone())
                .result()
                .await
                .unwrap();

            if let Some(out_path) = output_map.get("out") {
                output_paths.insert(path, out_path.clone());
            }
        } else {
            output_paths.insert(path.clone(), path);
        }
    }

    let valid_paths = match store
        .query_valid_paths(output_paths.values().clone(), true)
        .result()
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(e.to_string()),
    };

    let missing = output_paths
        .into_iter()
        .filter(|(_, v)| !valid_paths.contains(v))
        .map(|(k, _)| k)
        .collect::<Vec<String>>();

    Ok(missing)
}

pub fn get_buildlog_stream(
    server: MServer,
    build: MBuild,
    public_key: String,
    private_key: String,
) -> Result<Pin<Box<dyn Stream<Item = BuildLogStreamResponse> + Send>>, String> {
    let stream = async_stream::stream! {
        let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();
        let mut session = AsyncSession::<TokioTcpStream>::connect(server_addr, None).await.unwrap();

        init_session(&mut session, server.username.as_str(), public_key, private_key).await.unwrap();

        let mut channel = session.channel_session().await.unwrap();

        let command = format!("watch -n 0.5 nix-store --read-log {}", build.derivation_path);

        channel.exec(command.as_str()).await.unwrap();

        let mut buffer = [0; 1024];
        let mut log = String::new();

        loop {
            let len = channel.read(&mut buffer).await.unwrap();

            if len == 0 {
                break;
            }

            log.push_str(std::str::from_utf8(&buffer[..len]).unwrap());

            yield BuildLogStreamResponse {
                build_id: build.id,
                log: log.clone(),
            };
        }
    };

    Ok(Box::pin(stream))
}

pub async fn get_local_store(organization: Option<MOrganization>) -> Result<LocalNixStore, String> {
    if organization.is_none() || organization.clone().unwrap().use_nix_store {
        let store = DaemonStore::builder()
            .init(
                UnixStream::connect("/nix/var/nix/daemon-socket/socket")
                    .await
                    .unwrap(),
            )
            .await
            .map_err(|_| "Failed to Connect to Local Nix Deamon".to_string())?;

        // let nix_path_hashmap = HashMap::new();
        // nix_path_hashmap.insert(

        //     "NIX_PATH".to_string(),
        //     "/nix/var/nix/profiles/per-user/root/channels".to_string(),
        // );

        // store.set_options(nix_daemon::ClientSettings { keep_failed: (), keep_going: (), try_fallback: (), verbosity: (), max_build_jobs: (), max_silent_time: (), verbose_build: (), build_cores: (), use_substitutes: (), overrides:  }
        // }

        Ok(LocalNixStore::UnixStream(store))
    } else {
        let nix_store_dir = format!("/var/lib/gradient/store/{}", organization.unwrap().id);
        let mut child = Command::new("nix-store")
            .arg("--eval-store")
            .arg(nix_store_dir.clone())
            .arg("--serve")
            .env("NIX_STORE_DIR", nix_store_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|_| "Failed to Connect to Local Nix Deamon".to_string())?;

        let stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let _stderr = child.stderr.take().expect("Failed to open stderr");

        let duplex = CommandDuplex { stdin, stdout };

        let store = DaemonStore::builder().init(duplex).await.unwrap();

        Ok(LocalNixStore::CommandDuplex(store))
    }
}

pub fn get_builds_path(builds: Vec<&MBuild>) -> Vec<&str> {
    builds
        .iter()
        .map(|build| build.derivation_path.as_str())
        .collect()
}

pub async fn get_derivation<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    path: String,
    store: &mut DaemonStore<A>,
) -> Result<PathInfo, String> {
    Ok(store
        .query_pathinfo(path)
        .result()
        .await
        .map_err(|e| e.to_string())?
        .unwrap())
}

pub async fn get_output_path<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    path: String,
    store: &mut DaemonStore<A>,
) -> Result<Vec<String>, String> {
    match store
        .query_derivation_output_map(path.clone())
        .result()
        .await
        .map_err(|e| e.to_string())
    {
        Ok(output_map) => Ok(output_map.values().cloned().collect()),
        Err(e) => Err(format!("Failed to get output path for {}: {}", path, e)),
    }
}

pub async fn get_pathinfo<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    path: String,
    store: &mut DaemonStore<A>,
) -> Result<Option<nix_daemon::PathInfo>, String> {
    Ok(store
        .query_pathinfo(path)
        .result()
        .await
        .map_err(|e| e.to_string())
        .unwrap())
}

#[derive(Debug, Clone)]
pub struct BuildOutputInfo {
    pub path: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
}

pub async fn get_build_outputs_from_derivation<A: AsyncReadExt + AsyncWriteExt + Unpin + Send>(
    derivation_path: String,
    store: &mut DaemonStore<A>,
) -> Result<Vec<BuildOutputInfo>, String> {
    let output_map = store
        .query_derivation_output_map(derivation_path)
        .result()
        .await
        .map_err(|e| e.to_string())?;

    let mut outputs = Vec::new();

    for (_output_name, output_path) in output_map {
        if let Some(path_info) = store
            .query_pathinfo(output_path.clone())
            .result()
            .await
            .map_err(|e| e.to_string())?
        {
            let (hash, package) = get_hash_from_path(output_path.clone())
                .map_err(|e| format!("Failed to parse path {}: {}", output_path, e))?;

            outputs.push(BuildOutputInfo {
                path: output_path,
                hash,
                package,
                ca: path_info.ca,
            });
        }
    }

    Ok(outputs)
}
