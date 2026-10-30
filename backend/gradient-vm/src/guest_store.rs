/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gradient_wire::guest::GUEST_STORE_VSOCK_PORT;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tracing::warn;

use crate::vsock::connect_vsock;

const GUEST_STORE_SOCKET: &str = "guest-store.sock";

pub struct GuestStorePassthrough {
    socket: PathBuf,
    _accept: JoinSet<()>,
}

impl GuestStorePassthrough {
    pub fn open(guest_dir: &Path, vsock: PathBuf) -> Result<Self> {
        let socket = guest_dir.join(GUEST_STORE_SOCKET);
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("listen on {}", socket.display()))?;
        let mut accept = JoinSet::new();
        accept.spawn(pass_connections(listener, vsock));
        Ok(Self {
            socket,
            _accept: accept,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

async fn pass_connections(listener: UnixListener, vsock: PathBuf) {
    let mut connections = JoinSet::new();
    while let Ok((client, _)) = listener.accept().await {
        connections.spawn(pass_through(client, vsock.clone()));
    }
}

async fn pass_through(mut client: UnixStream, vsock: PathBuf) {
    let passed = async {
        let mut guest = connect_vsock(&vsock, GUEST_STORE_VSOCK_PORT).await?;
        tokio::io::copy_bidirectional(&mut client, &mut guest).await?;
        anyhow::Ok(())
    };
    if let Err(e) = passed.await {
        warn!(error = %format!("{e:#}"), "guest store connection ended");
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn a_store_connection_reaches_the_guest_store_port() {
        let dir = tempfile::tempdir().unwrap();
        let vsock = dir.path().join("vsock.sock");
        let hypervisor = UnixListener::bind(&vsock).unwrap();
        let guest = async move {
            let (stream, _) = hypervisor.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut connect = String::new();
            stream.read_line(&mut connect).await.unwrap();
            stream
                .get_mut()
                .write_all(b"OK 1073741824\n")
                .await
                .unwrap();
            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).await.unwrap();
            stream.get_mut().write_all(&request).await.unwrap();
            connect
        };
        let passthrough = GuestStorePassthrough::open(dir.path(), vsock).unwrap();
        let client = async {
            let mut client = UnixStream::connect(passthrough.socket()).await.unwrap();
            client.write_all(b"nix\0").await.unwrap();
            let mut answer = [0_u8; 4];
            client.read_exact(&mut answer).await.unwrap();
            answer
        };

        let (connect, answer) = tokio::join!(guest, client);

        assert_eq!(&answer, b"nix\0");
        assert_eq!(connect, format!("CONNECT {GUEST_STORE_VSOCK_PORT}\n"));
    }
}
