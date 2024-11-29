use nix_daemon::{self, BuildMode, BuildResult, Progress, Store, PathInfo};
use nix_daemon::nix::DaemonStore;
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
};
use std::path::PathBuf;
use std::collections::HashMap;

use super::types::*;


pub async fn connect(url: SocketAddr, store_path: Option<String>) -> Result<NixStore, Box<dyn std::error::Error + Send + Sync>> {
    let mut session = AsyncSession::<TokioTcpStream>::connect(url, None).await?;
    let private_key: PathBuf = PathBuf::from("/home/dennis/.ssh/keys/github");

    init_session(&mut session, "dennis", private_key).await?;

    let mut channel = session.channel_session().await?;

    let command = if let Some(path) = store_path {
        format!("nix-daemon --stdio --option store {}", path)
    } else {
        "nix-daemon --stdio".to_string()
    };

    channel.exec(command.as_str()).await?;

    Ok(DaemonStore::builder().init(channel).await?)
}

pub async fn init_session(session: &mut AsyncSession<TokioTcpStream>, username: &str, private_key: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    session.handshake().await.unwrap_or_else(|err| {
        println!("Handshake failed: {:?}", err);
    });

    session.userauth_pubkey_file(username, None, &private_key, None).await?;
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
>(builds: Vec<&MBuild>, from_store: &mut DaemonStore<A>, to_store: &mut DaemonStore<B>, remote_store_uri: SocketAddr, local_is_receiver: bool) {
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
                    .arg(format!("ssh://{}:{}", remote_store_uri.ip(), remote_store_uri.port()))
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
