/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod connection;
mod target;
#[cfg(test)]
pub(crate) mod tests;

use super::git_credentials::CURRENT_THREAD_GIT_CREDENTIALS;
use connection::SshCommandStream;
use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use gradient_types::SecretString;
use gradient_wire::messages::{GitServerSshHostKey, GitSshHostKeyObservation};
use russh::keys::PublicKey;
use std::sync::{Arc, Mutex, Once, PoisonError};
use target::SshTarget;

pub use connection::fetch_ssh_host_key;

const SCHEMES: [&str; 3] = ["ssh", "ssh+git", "git+ssh"];

#[derive(Clone, Debug)]
pub struct SshGitPrivateKeyKnownHostsAndObservations {
    private_key: SecretString,
    host_keys: Vec<GitServerSshHostKey>,
    observations: Arc<Mutex<Vec<GitSshHostKeyObservation>>>,
}

impl SshGitPrivateKeyKnownHostsAndObservations {
    pub fn new(private_key: SecretString, host_keys: Vec<GitServerSshHostKey>) -> Self {
        Self {
            private_key,
            host_keys,
            observations: Arc::default(),
        }
    }

    pub fn observations(&self) -> Vec<GitSshHostKeyObservation> {
        std::mem::take(
            &mut *self
                .observations
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    fn record_host_key_observation(&self, observation: GitSshHostKeyObservation) {
        self.observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(observation);
    }

    fn find_parsed_saved_host_public_key_for_host_and_port(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Option<PublicKey>, String> {
        let host = host.to_lowercase();
        self.host_keys
            .iter()
            .find(|saved| saved.host.to_lowercase() == host && saved.port == port)
            .map(|saved| {
                PublicKey::from_openssh(&saved.public_key).map_err(|error| {
                    format!(
                        "Host key stored for {host}:{port} is not a valid OpenSSH public key ({error})."
                    )
                })
            })
            .transpose()
    }
}

pub(super) fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        for scheme in SCHEMES {
            // SAFETY: the Once keeps registration away from transports created by this crate.
            if let Err(error) = unsafe { git2::transport::register(scheme, transport) } {
                tracing::error!(%error, scheme, "failed to register the SSH transport");
            }
        }
    });
}

fn transport(remote: &git2::Remote<'_>) -> Result<Transport, git2::Error> {
    let credentials = CURRENT_THREAD_GIT_CREDENTIALS
        .with_borrow(|credentials| credentials.ssh_git_private_key_and_known_hosts.clone());
    Transport::smart(remote, false, SshSubtransport { credentials })
}

struct SshSubtransport {
    credentials: Option<SshGitPrivateKeyKnownHostsAndObservations>,
}

impl SmartSubtransport for SshSubtransport {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let program = match action {
            Service::UploadPackLs => "git-upload-pack",
            Service::ReceivePackLs => "git-receive-pack",
            Service::UploadPack | Service::ReceivePack => {
                return Err(git2::Error::from_str("SSH stream requested twice"));
            }
        };

        let credentials = self
            .credentials
            .as_ref()
            .ok_or_else(|| git2::Error::from_str("SSH remote needs a private key"))?;
        let key = russh::keys::decode_secret_key(credentials.private_key.expose(), None)
            .map_err(|e| git2::Error::from_str(&format!("invalid SSH private key: {e}")))?;
        let target = SshTarget::parse(url).map_err(|e| git2::Error::from_str(&e.to_string()))?;
        let command = target.command(program);

        SshCommandStream::open(&target, key, command, credentials)
            .map(|stream| Box::new(stream) as Box<dyn SmartSubtransportStream>)
            .map_err(|e| git2::Error::from_str(&format!("SSH to {}: {e}", target.host)))
    }

    fn close(&self) -> Result<(), git2::Error> {
        Ok(())
    }
}
