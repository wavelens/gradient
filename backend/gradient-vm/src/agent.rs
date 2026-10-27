/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use gradient_wire::guest::{AGENT_VSOCK_PORT, AgentToWorker, GuestConnection};
use tokio::net::UnixStream;
use tracing::debug;

use crate::vsock::connect_vsock;

pub const GUEST_BOOT_TIMEOUT: Duration = Duration::from_secs(120);
const RETRY: Duration = Duration::from_millis(500);

pub struct ReadyAgent {
    pub conn: GuestConnection<UnixStream>,
    pub agent_version: String,
}

#[derive(Debug, Clone)]
pub struct GuestBootError(pub String);

impl fmt::Display for GuestBootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GuestBootError {}

pub async fn wait_ready(vsock: &Path, deadline: Duration) -> Result<ReadyAgent, GuestBootError> {
    let attempts = async {
        loop {
            match ready_once(vsock).await {
                Ok(ready) => return ready,
                Err(e) => debug!(error = %e, "guest agent not ready yet"),
            }
            tokio::time::sleep(RETRY).await;
        }
    };
    tokio::time::timeout(deadline, attempts).await.map_err(|_| {
        GuestBootError(format!(
            "guest agent did not answer within {}s",
            deadline.as_secs()
        ))
    })
}

async fn ready_once(vsock: &Path) -> Result<ReadyAgent> {
    let stream = connect_vsock(vsock, AGENT_VSOCK_PORT).await?;
    let mut conn = GuestConnection::agree(stream).await?;
    match conn.recv::<AgentToWorker>().await? {
        Some(AgentToWorker::Ready { agent_version }) => Ok(ReadyAgent {
            conn,
            agent_version,
        }),
        Some(other) => bail!("guest agent sent {other:?} before Ready"),
        None => bail!("guest agent closed before Ready"),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn no_agent_within_the_deadline_is_a_boot_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("vsock.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let silent_agent = async move {
            let mut open = Vec::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                stream.write_all(b"OK 1\n").await.unwrap();
                open.push(stream);
            }
        };

        let started = tokio::time::Instant::now();
        let result = tokio::select! {
            result = wait_ready(&socket, GUEST_BOOT_TIMEOUT) => result,
            () = silent_agent => unreachable!(),
        };
        assert!(result.is_err());
        assert!(started.elapsed() >= GUEST_BOOT_TIMEOUT);
    }
}
