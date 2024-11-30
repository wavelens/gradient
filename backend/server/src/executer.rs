use nix_daemon::{self, BuildMode, BuildResult, Progress, Store, PathInfo};
use nix_daemon::nix::DaemonStore;
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
};
use std::collections::HashMap;
use futures::Stream;
use uuid::Uuid;
use serde::Serialize;
use std::pin::Pin;

use super::types::*;
use super::input;


#[derive(Debug, Clone, Serialize)]
pub struct BuildLogStreamResponse {
    pub build_id: Uuid,
    pub log: String,
}

pub async fn connect(server: MServer, store_path: Option<String>) -> Result<NixStore, Box<dyn std::error::Error + Send + Sync>> {
    let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();
    let mut session = AsyncSession::<TokioTcpStream>::connect(server_addr, None).await?;

    init_session(&mut session, server.username.as_str(), server.public_key, server.private_key).await?;

    let mut channel = session.channel_session().await?;

    let command = if let Some(path) = store_path {
        format!("nix-daemon --stdio --option store {}", path)
    } else {
        "nix-daemon --stdio".to_string()
    };

    channel.exec(command.as_str()).await?;

    Ok(DaemonStore::builder().init(channel).await?)
}

pub async fn init_session(session: &mut AsyncSession<TokioTcpStream>, username: &str, public_key: String, private_key: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    session.handshake().await.unwrap_or_else(|err| {
        println!("Handshake failed: {:?}", err);
    });

    session.userauth_pubkey_memory(username, Some(public_key.as_str()), private_key.as_str(), None).await?;
    assert!(session.authenticated());

    Ok(())
}

pub async fn execute_build(builds: Vec<&MBuild>, remote_store: &mut NixStore) -> Result<HashMap<String, BuildResult>, Box<dyn std::error::Error + Send + Sync>> {
    println!("Executing builds");

    let paths = get_builds_path(builds.clone());

    for path in paths.clone() {
        remote_store.ensure_path(path).result().await?;
    }

    let paths = paths.iter().map(|p| format!("{}!*", p).to_string()).collect::<Vec<String>>();
    remote_store.build_paths_with_results(paths, BuildMode::Normal).result().await.map_err(|e| e.into())
}


pub async fn copy_builds<
    A: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    B: AsyncReadExt + AsyncWriteExt + Unpin + Send
>(builds: Vec<&MBuild>, from_store: &mut DaemonStore<A>, to_store: &mut DaemonStore<B>, server: MServer, local_is_receiver: bool) {
    let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();
    for build in builds {
        let path = build.derivation_path.as_str();

        println!("Copying build {} to {}", build.id, if local_is_receiver { "local" } else { "remote" });

        let path_info = match from_store.query_pathinfo(path).result().await.unwrap() {
            Some(path_info) => path_info,
            None => continue,
        };

        for ref_path in path_info.references.iter().chain(std::iter::once(&path.to_string())) {
            if !to_store.is_valid_path(ref_path).result().await.unwrap() {
                Command::new("nix")
                    .arg("copy")
                    .arg(if local_is_receiver { "--from" } else { "--to" })
                    .arg(format!("ssh://{}:{}", server_addr.ip(), server_addr.port()))
                    .arg(ref_path)
                    .status()
                    .await.unwrap();
            }
        }
    }
}

pub async fn get_missing_builds<
    A: AsyncReadExt + AsyncWriteExt + Unpin + Send
>(paths: Vec<String>, store: &mut DaemonStore<A>) -> Result<Vec<String>, String> {
    let mut output_paths: HashMap<String, String> = HashMap::new();

    for path in paths {
        if path.ends_with(".drv") {
            let output_map = store.query_derivation_output_map(path.clone()).result().await.unwrap();

            if let Some(out_path) = output_map.get("out") {
                output_paths.insert(path, out_path.clone());
            }
        } else {
            output_paths.insert(path.clone(), path);
        }
    }

    let valid_paths = store.query_valid_paths(output_paths.values().clone(), true).result().await.unwrap();

    let missing = output_paths.into_iter()
        .filter(|(_, v)| !valid_paths.contains(v))
        .map(|(k, _)| k).collect::<Vec<String>>();

    Ok(missing)
}

pub fn get_buildlog_stream(server: MServer, build: MBuild) -> Result<Pin<Box<dyn Stream<Item = BuildLogStreamResponse> + Send>>, String> {
    let stream = async_stream::stream! {
        let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();
        let mut session = AsyncSession::<TokioTcpStream>::connect(server_addr, None).await.unwrap();

        init_session(&mut session, server.username.as_str(), server.public_key, server.private_key).await.unwrap();

        let mut channel = session.channel_session().await.unwrap();

        let command = format!("nix-store --log {}", build.derivation_path);

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

pub async fn get_local_store() -> DaemonStore<UnixStream> {
    DaemonStore::builder().init(UnixStream::connect("/nix/var/nix/daemon-socket/socket").await.unwrap()).await.unwrap()
}

pub fn get_builds_path(builds: Vec<&MBuild>) -> Vec<&str> {
    builds.iter().map(|build| build.derivation_path.as_str()).collect()
}

pub async fn get_derivation<
    A: AsyncReadExt + AsyncWriteExt + Unpin + Send
>(path: String, store: &mut DaemonStore<A>) -> Result<PathInfo, String> {
    Ok(store.query_pathinfo(path).result().await.map_err(|e| e.to_string())?.unwrap())
}

pub async fn get_output_path<
    A: AsyncReadExt + AsyncWriteExt + Unpin + Send
>(path: String, store: &mut DaemonStore<A>) -> Result<Vec<String>, String> {
    Ok(store.query_derivation_output_map(path).result().await.map_err(|e| e.to_string()).unwrap().values().cloned().collect())
}
