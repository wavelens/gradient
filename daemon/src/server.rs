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
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

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
    loop {
        let (stream, _) = listener.accept().await?;
        let uid = stream.peer_cred().ok().map(|c| c.uid());
        serve_stream(&backend, next_conn(uid), stream);
    }
}

pub fn serve_stream<B, S>(backend: &Arc<B>, conn: ConnInfo, stream: S) -> JoinHandle<()>
where
    B: Backend,
    S: AsyncRead + AsyncWrite + Debug + Send + 'static,
{
    let handler = backend.handler(conn);
    tokio::spawn(async move {
        let (read, write) = tokio::io::split(stream);
        let served = Builder::new()
            .set_store_dir(StoreDir::default())
            .serve_connection(read, write, handler)
            .await;
        if let Err(error) = served {
            tracing::debug!(conn = conn.id, %error, "connection ended with error");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullHandler;
    use crate::journal::Journal;
    use harmonia_store_remote::{DaemonClient, DaemonStore as _};

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
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        serve_stream(
            &Arc::new(Null(Journal::new())),
            next_conn(None),
            server_side,
        );
        let (read, write) = tokio::io::split(client_side);
        let mut client = DaemonClient::builder()
            .connect(read, write)
            .await
            .expect("handshake");
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
