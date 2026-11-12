/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Credentials never land on disk. They arrive as separate frames right before the job assignment that they belong to. CredentialStore can buffer them for a single connection. Each job assignment can retrieve its own set, so a completed job won't delete credentials of another job. SecretBytes can lock memory pages with mlock(2) and zero them on drop.

use gradient_sources::SshGitPrivateKeyKnownHostsAndObservations;
use gradient_types::{SecretBytes, SecretString};
use gradient_wire::messages::{CredentialKind, GitServerSshHostKey};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct CredentialStore {
    ssh_key: Option<SecretBytes>,
    ssh_host_keys: Vec<GitServerSshHostKey>,
    job_secrets: BTreeMap<(String, String), SecretBytes>,
}

impl CredentialStore {
    pub fn store(&mut self, kind: CredentialKind, data: SecretBytes) {
        match kind {
            CredentialKind::SshKey => {
                self.ssh_key = Some(data);
            }
            CredentialKind::JobSecret { job_id, name } => {
                self.job_secrets.insert((job_id, name), data);
            }
            CredentialKind::SshHostKeys { keys } => {
                self.ssh_host_keys = keys;
            }
        }
    }

    pub fn take_credentials_for_job(&mut self, job_id: &str) -> JobCredentials {
        let (own, others) = std::mem::take(&mut self.job_secrets)
            .into_iter()
            .partition(|((owner, _), _)| owner == job_id);
        self.job_secrets = others;
        JobCredentials {
            ssh_key: self.ssh_key.take(),
            ssh_host_keys: std::mem::take(&mut self.ssh_host_keys),
            secrets: own
                .into_iter()
                .map(|((_, name), value)| (name, value))
                .collect(),
        }
    }
}

#[derive(Clone, Default)]
pub struct JobCredentials {
    ssh_key: Option<SecretBytes>,
    ssh_host_keys: Vec<GitServerSshHostKey>,
    secrets: BTreeMap<String, SecretBytes>,
}

impl JobCredentials {
    pub fn ssh_git_private_key_known_hosts_and_observations(
        &self,
    ) -> Option<SshGitPrivateKeyKnownHostsAndObservations> {
        let private_key = SecretString::from_utf8(self.ssh_key.clone()?)?;
        Some(SshGitPrivateKeyKnownHostsAndObservations::new(
            private_key,
            self.ssh_host_keys.clone(),
        ))
    }

    pub fn secret_value_by_name(&self, name: &str) -> Option<SecretBytes> {
        self.secrets.get(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_secret(job_id: &str, name: &str) -> CredentialKind {
        CredentialKind::JobSecret {
            job_id: job_id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn a_job_keeps_its_credentials_after_the_store_moves_on() {
        let mut store = CredentialStore::default();
        store.store(CredentialKind::SshKey, b"p1".to_vec().into());
        store.store(job_secret("exec:1", "wandb-token"), b"v1".to_vec().into());

        let held = store.take_credentials_for_job("exec:1");
        store.store(CredentialKind::SshKey, b"p2".to_vec().into());

        assert_eq!(held.ssh_key.as_ref().expect("kept").expose(), b"p1");
        assert_eq!(
            held.secret_value_by_name("wandb-token")
                .expect("kept")
                .expose(),
            b"v1"
        );
    }

    #[test]
    fn a_later_job_without_a_key_gets_none_of_the_previous_key() {
        let mut store = CredentialStore::default();
        store.store(CredentialKind::SshKey, b"p1".to_vec().into());
        let _first = store.take_credentials_for_job("exec:1");

        assert!(store.take_credentials_for_job("exec:2").ssh_key.is_none());
    }

    #[test]
    fn host_keys_travel_with_the_ssh_key_into_job_credentials() {
        let keys = vec![GitServerSshHostKey {
            host: "git.elektrat.de".into(),
            port: 22,
            public_key: "ssh-ed25519 AAAA".into(),
        }];
        let mut store = CredentialStore::default();
        store.store(CredentialKind::SshKey, b"p1".to_vec().into());
        store.store(
            CredentialKind::SshHostKeys { keys: keys.clone() },
            Vec::<u8>::new().into(),
        );

        let held = store.take_credentials_for_job("exec:1");

        assert_eq!(held.ssh_host_keys, keys);
        assert!(
            held.ssh_git_private_key_known_hosts_and_observations()
                .is_some()
        );
        assert!(
            store
                .take_credentials_for_job("exec:2")
                .ssh_host_keys
                .is_empty()
        );
    }

    #[test]
    fn taking_one_job_leaves_the_secrets_of_another_job() {
        let mut store = CredentialStore::default();
        store.store(job_secret("exec:1", "wandb-token"), b"v1".to_vec().into());
        store.store(job_secret("exec:2", "wandb-token"), b"v2".to_vec().into());

        let first = store.take_credentials_for_job("exec:1");
        let second = store.take_credentials_for_job("exec:2");

        assert_eq!(
            first.secret_value_by_name("wandb-token").unwrap().expose(),
            b"v1"
        );
        assert_eq!(
            second.secret_value_by_name("wandb-token").unwrap().expose(),
            b"v2"
        );
    }
}
