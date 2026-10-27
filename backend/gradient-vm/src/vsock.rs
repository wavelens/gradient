/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const MAX_REPLY: usize = 64;

pub async fn connect_vsock(socket: &Path, port: u32) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to {}", socket.display()))?;
    stream
        .write_all(format!("CONNECT {port}\n").as_bytes())
        .await?;

    let mut reply = Vec::new();
    loop {
        let byte = stream
            .read_u8()
            .await
            .with_context(|| format!("vsock port {port} closed before the handshake"))?;
        if byte == b'\n' {
            break;
        }
        reply.push(byte);
        if reply.len() > MAX_REPLY {
            bail!("vsock port {port}: handshake reply too long");
        }
    }

    let reply = String::from_utf8_lossy(&reply);
    if !reply.starts_with("OK ") {
        bail!("vsock port {port} refused: {reply}");
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;

    use super::*;

    #[tokio::test]
    async fn a_refused_vsock_connect_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("vsock.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let host = async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(line, "CONNECT 7000\n");
            stream.write_all(b"ERR\n").await.unwrap();
        };

        let (result, ()) = tokio::join!(connect_vsock(&socket, 7000), host);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("refused"), "{error}");
    }
}
