/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SshGitPrivateKeyKnownHostsAndObservations;
use super::target::SshTarget;
use crate::SourceError;
use crate::git::git_network_limits::STALLED_SERVER_TIMEOUT;
use gradient_wire::messages::{GitServerSshHostKey, GitSshHostKeyObservation};
use russh::ChannelStream;
use russh::client::{self, Handle, Msg};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use std::borrow::Cow;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;

struct ProjectSshHostKeyTrustOnFirstUse {
    host: String,
    port: u16,
    saved: Option<PublicKey>,
    credentials: SshGitPrivateKeyKnownHostsAndObservations,
}

impl client::Handler for ProjectSshHostKeyTrustOnFirstUse {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        match &self.saved {
            None => {
                self.credentials.record_host_key_observation(
                    GitSshHostKeyObservation::UnknownHostKeyAcceptedForSaving {
                        key: GitServerSshHostKey {
                            host: self.host.clone(),
                            port: self.port,
                            public_key: key.to_openssh()?,
                        },
                    },
                );
                Ok(true)
            }
            Some(saved) if saved.key_data() == key.key_data() => Ok(true),
            Some(_) => {
                self.credentials.record_host_key_observation(
                    GitSshHostKeyObservation::HostKeyMismatchConnectionRefused {
                        host: self.host.clone(),
                        port: self.port,
                    },
                );
                Ok(false)
            }
        }
    }
}

pub(super) struct SshCommandStream {
    stream: ChannelStream<Msg>,
    _session: Handle<ProjectSshHostKeyTrustOnFirstUse>,
    runtime: Runtime,
}

impl SshCommandStream {
    pub fn open(
        target: &SshTarget,
        key: PrivateKey,
        command: String,
        credentials: &SshGitPrivateKeyKnownHostsAndObservations,
    ) -> io::Result<Self> {
        let saved = credentials
            .find_parsed_saved_host_public_key_for_host_and_port(&target.host, target.port)
            .map_err(io::Error::other)?;
        let verifier = ProjectSshHostKeyTrustOnFirstUse {
            host: target.host.to_lowercase(),
            port: target.port,
            saved,
            credentials: credentials.clone(),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let (session, stream) = runtime
            .block_on(run(target, key, command, verifier))
            .map_err(|error| match error {
                russh::Error::UnknownKey => io::Error::other(format!(
                    "Host key changed for {}:{}. Check the new key in project settings.",
                    target.host, target.port
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
    verifier: ProjectSshHostKeyTrustOnFirstUse,
) -> Result<(Handle<ProjectSshHostKeyTrustOnFirstUse>, ChannelStream<Msg>), russh::Error> {
    let mut preferred = gradient_util::ssh::ALGORITHMS;
    if let Some(saved) = &verifier.saved {
        preferred.key = Cow::Owned(vec![saved.algorithm()]);
    }
    let config = Arc::new(client::Config {
        preferred,
        ..Default::default()
    });
    let mut session = connect_and_record_changed_host_key_if_saved_algorithm_is_unsupported(
        config, target, verifier,
    )
    .await?;

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

async fn connect_and_record_changed_host_key_if_saved_algorithm_is_unsupported(
    config: Arc<client::Config>,
    target: &SshTarget,
    verifier: ProjectSshHostKeyTrustOnFirstUse,
) -> Result<Handle<ProjectSshHostKeyTrustOnFirstUse>, russh::Error> {
    let narrowed = verifier.saved.is_some();
    let observation = GitSshHostKeyObservation::HostKeyMismatchConnectionRefused {
        host: verifier.host.clone(),
        port: verifier.port,
    };
    let credentials = verifier.credentials.clone();
    client::connect(config, (target.host.as_str(), target.port), verifier)
        .await
        .map_err(|error| match error {
            russh::Error::NoCommonAlgo {
                kind: russh::AlgorithmKind::Key,
                ..
            } if narrowed => {
                credentials.record_host_key_observation(observation);
                russh::Error::UnknownKey
            }
            error => error,
        })
}

struct PresentedHostKeyCaptureClient {
    presented: Arc<Mutex<Option<PublicKey>>>,
}

impl client::Handler for PresentedHostKeyCaptureClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if let PublicKeyOrCertificate::PublicKey { key, .. } = key {
            *self
                .presented
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(key.clone());
        }
        Ok(false)
    }
}

pub async fn fetch_ssh_host_key(host: &str, port: u16) -> Result<String, SourceError> {
    let failed = |reason: String| SourceError::SshHostKeyReadFailed { reason };
    let presented = Arc::new(Mutex::new(None));
    let config = Arc::new(client::Config {
        preferred: gradient_util::ssh::ALGORITHMS,
        ..Default::default()
    });
    let client = PresentedHostKeyCaptureClient {
        presented: presented.clone(),
    };
    let connection = tokio::time::timeout(
        STALLED_SERVER_TIMEOUT,
        client::connect(config, (host, port), client),
    )
    .await
    .map_err(|_| {
        failed(format!(
            "{host}:{port} did not send a host key within {} seconds",
            STALLED_SERVER_TIMEOUT.as_secs()
        ))
    })?;
    let presented = presented
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    match (presented, connection) {
        (Some(key), _) => key.to_openssh().map_err(|e| failed(e.to_string())),
        (None, Err(error)) => Err(failed(error.to_string())),
        (None, Ok(_)) => Err(failed(format!(
            "{host}:{port} closed the connection before sending a host key"
        ))),
    }
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
