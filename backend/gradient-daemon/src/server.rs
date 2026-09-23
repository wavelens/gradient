/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::backend::{Backend, ConnInfo};
use harmonia_daemon::server::Builder;
use harmonia_store_path::StoreDir;
use std::fmt::Debug;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub fn next_conn(uid: Option<u32>) -> ConnInfo {
    ConnInfo {
        id: NEXT_CONN.fetch_add(1, Ordering::Relaxed),
        uid,
    }
}

pub async fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

pub async fn serve<B: Backend>(backend: Arc<B>, socket: &Path) -> anyhow::Result<()> {
    let listener = bind(socket).await?;
    tracing::info!(socket = %socket.display(), "listening");
    accept_each(listener, |stream| {
        let uid = stream.peer_cred().ok().map(|c| c.uid());
        serve_stream(backend.clone(), next_conn(uid), stream)
    })
    .await
}

pub async fn accept_each<F, Fut>(listener: UnixListener, mut serve: F) -> anyhow::Result<()>
where
    F: FnMut(UnixStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                connections.spawn(serve(accepted?.0));
            }
            Some(joined) = connections.join_next() => {
                if let Err(error) = joined {
                    tracing::warn!(%error, "connection task failed");
                }
            }
        }
    }
}

pub async fn serve_stream<B, S>(backend: Arc<B>, conn: ConnInfo, stream: S)
where
    B: Backend,
    S: AsyncRead + AsyncWrite + Debug + Send + 'static,
{
    let handler = backend.handler(conn);
    let (read, write) = tokio::io::split(stream);
    let served = Builder::new()
        .set_store_dir(StoreDir::default())
        .serve_connection(read, write, handler)
        .await;
    if let Err(error) = served {
        tracing::debug!(conn = conn.id, %error, "connection ended with error");
    }
}

#[cfg(test)]
pub type TestClient = harmonia_store_remote::DaemonClient<
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
>;

#[cfg(test)]
pub async fn connect_duplex<B: Backend>(backend: &Arc<B>) -> (JoinSet<()>, TestClient) {
    let (client_side, server_side) = tokio::io::duplex(1 << 20);
    let mut server = JoinSet::new();
    server.spawn(serve_stream(backend.clone(), next_conn(None), server_side));
    let (read, write) = tokio::io::split(client_side);
    let client = harmonia_store_remote::DaemonClient::builder()
        .connect(read, write)
        .await
        .expect("handshake");
    (server, client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullHandler;
    use crate::journal::Journal;
    use harmonia_store_remote::DaemonStore as _;

    struct Null(Journal);

    impl Backend for Null {
        type Handler = NullHandler;

        fn journal(&self) -> &Journal {
            &self.0
        }

        fn handler(self: &Arc<Self>, _conn: ConnInfo) -> NullHandler {
            NullHandler
        }

        fn control(
            &self,
            _: &str,
            _: &serde_json::Value,
        ) -> Option<anyhow::Result<serde_json::Value>> {
            None
        }
    }

    #[tokio::test]
    async fn client_handshakes_and_gets_unimplemented() {
        let (_server, mut client) = connect_duplex(&Arc::new(Null(Journal::new()))).await;
        let path =
            harmonia_store_path::StorePath::from_base_path("00000000000000000000000000000000-x")
                .expect("path");
        let err = client
            .is_valid_path(&path)
            .await
            .expect_err("unimplemented");
        assert!(err.to_string().contains("unimplemented"), "{err}");
    }
}
