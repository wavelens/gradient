/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::target::SshTarget;
use russh::ChannelStream;
use russh::client::{self, Handle, Msg};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::io::{self, Read, Write};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;

struct AnyHostKey;

impl client::Handler for AnyHostKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub(super) struct SshCommandStream {
    stream: ChannelStream<Msg>,
    _session: Handle<AnyHostKey>,
    runtime: Runtime,
}

impl SshCommandStream {
    pub fn open(target: &SshTarget, key: PrivateKey, command: String) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let (session, stream) = runtime
            .block_on(run(target, key, command))
            .map_err(io::Error::other)?;

        Ok(Self {
            stream,
            _session: session,
            runtime,
        })
    }
}

async fn run(
    target: &SshTarget,
    key: PrivateKey,
    command: String,
) -> Result<(Handle<AnyHostKey>, ChannelStream<Msg>), russh::Error> {
    let config = Arc::new(client::Config {
        preferred: gradient_util::ssh::ALGORITHMS,
        ..Default::default()
    });
    let mut session =
        client::connect(config, (target.host.as_str(), target.port), AnyHostKey).await?;

    let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
    if !session
        .authenticate_publickey(&target.user, key)
        .await?
        .success()
    {
        return Err(russh::Error::NotAuthenticated);
    }

    let channel = session.channel_open_session().await?;
    channel.exec(true, command).await?;
    Ok((session, channel.into_stream()))
}

impl Read for SshCommandStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.runtime.block_on(self.stream.read(buf))
    }
}

impl Write for SshCommandStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.runtime.block_on(self.stream.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.runtime.block_on(self.stream.flush())
    }
}
