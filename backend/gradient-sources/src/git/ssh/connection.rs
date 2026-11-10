/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::target::SshTarget;
use crate::git::git_network_limits::STALLED_SERVER_TIMEOUT;
use russh::ChannelStream;
use russh::client::{self, Handle, Msg};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;

struct OpenSshKnownHostsHostKeyVerifier {
    host: String,
    port: u16,
    known_hosts_files: Vec<PathBuf>,
}

impl client::Handler for OpenSshKnownHostsHostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        for file in &self.known_hosts_files {
            if russh::keys::check_known_hosts_path(&self.host, self.port, key, file)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub(super) struct SshCommandStream {
    stream: ChannelStream<Msg>,
    _session: Handle<OpenSshKnownHostsHostKeyVerifier>,
    runtime: Runtime,
}

impl SshCommandStream {
    pub fn open(
        target: &SshTarget,
        key: PrivateKey,
        command: String,
        known_hosts_files: Vec<PathBuf>,
    ) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let (session, stream) = runtime
            .block_on(run(target, key, command, known_hosts_files))
            .map_err(|error| match error {
                russh::Error::UnknownKey => io::Error::other(format!(
                    "the host key for {} does not appear in /etc/ssh/ssh_known_hosts or ~/.ssh/known_hosts",
                    target.host
                )),
                error => io::Error::other(error),
            })?;

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
    known_hosts_files: Vec<PathBuf>,
) -> Result<(Handle<OpenSshKnownHostsHostKeyVerifier>, ChannelStream<Msg>), russh::Error> {
    let config = Arc::new(client::Config {
        preferred: gradient_util::ssh::ALGORITHMS,
        ..Default::default()
    });
    let verifier = OpenSshKnownHostsHostKeyVerifier {
        host: target.host.clone(),
        port: target.port,
        known_hosts_files,
    };
    let mut session =
        client::connect(config, (target.host.as_str(), target.port), verifier).await?;

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
        self.runtime
            .block_on(tokio::time::timeout(
                STALLED_SERVER_TIMEOUT,
                self.stream.read(buf),
            ))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "SSH server sent nothing for {} seconds",
                        STALLED_SERVER_TIMEOUT.as_secs()
                    ),
                )
            })?
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
