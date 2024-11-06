use nix_daemon::{self, BuildMode, Progress, Store};
use nix_daemon::nix::DaemonStore;
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
};
use std::path::PathBuf;

use super::tables::*;
use super::types::NixStore;


pub async fn connect(url: SocketAddr) -> Result<NixStore, Box<dyn std::error::Error>> {
    let mut session = AsyncSession::<TokioTcpStream>::connect(url, None).await?;
    let private_key: PathBuf = PathBuf::from("/home/dennis/.ssh/keys/github");

    init_session(&mut session, "dennis", private_key).await?;

    let mut channel = session.channel_session().await?;

    channel.exec("nix-daemon --stdio").await?;

    Ok(DaemonStore::builder().init(channel).await?)
}

pub async fn init_session(session: &mut AsyncSession<TokioTcpStream>, username: &str, private_key: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    session.handshake().await.unwrap_or_else(|err| {
        println!("Handshake failed: {:?}", err);
    });

    session.userauth_pubkey_file(username, None, &private_key, None).await?;
    assert!(session.authenticated());

    Ok(())
}

pub async fn execute_build(builds: Vec<&Build>, remote_store: &mut NixStore) {
    println!("Executing builds");

    let result = remote_store.build_paths_with_results(get_builds_path(builds), BuildMode::Normal).result().await;
}

pub async fn copy_builds<
    A: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    B: AsyncReadExt + AsyncWriteExt + Unpin + Send
>(builds: Vec<&Build>, from_store: &mut DaemonStore<A>, to_store: &mut DaemonStore<B>, remote_store_uri: SocketAddr, local_is_receiver: bool) {

    for build in builds {
        let path = build.path.as_str();
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

pub async fn get_missing_builds(build: &Build, store: &mut NixStore) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    println!("Querying missing builds");

    let result = store.query_missing(get_builds_path(vec![build])).result().await?;

    let missing = result.will_build.into_iter().chain(result.will_substitute.into_iter()).collect();

    Ok(missing)
}

pub async fn get_local_store() -> DaemonStore<UnixStream> {
    DaemonStore::builder().init(UnixStream::connect("/nix/var/nix/daemon-socket/socket").await.unwrap()).await.unwrap()
}

pub fn get_builds_path(builds: Vec<&Build>) -> Vec<&str> {
    builds.iter().map(|build| build.path.as_str()).collect()
}
