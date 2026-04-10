/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{GenericDaemonClient};
use crate::types::input as input;
use crate::types::*;
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use harmonia_store_remote::DaemonClientBuilder;
use tracing::error;

pub async fn connect(
    server: MServer,
    store_path: Option<String>,
    public_key: String,
    private_key: String,
) -> anyhow::Result<GenericDaemonClient<tokio::io::ReadHalf<BoxedIo>, tokio::io::WriteHalf<BoxedIo>>> {
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

    let io = BoxedIo::new(channel);
    let (reader, writer) = tokio::io::split(io);

    let client = DaemonClientBuilder::new()
        .connect(reader, writer)
        .await
        .map_err(|e| anyhow::anyhow!("Daemon handshake failed: {}", e))?;

    Ok(client)
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
