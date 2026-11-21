/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Credentials never reach disk. Job secrets arrive in a separate frame right before each job
//! assignment. SSH keys, host keys for SSH and Git credentials for HTTPS arrive inside that
//! assignment. Jobs hold credentials only for their own assignment. They never hold credentials
//! from a previous job. A `SecretBytes` buffer can lock its memory pages with mlock(2) and zero
//! them on drop.

use gradient_sources::{
    GitOperationCredentials, HttpsGitCredential, SshGitPrivateKeyKnownHostsAndObservations,
};
use gradient_types::{SecretBytes, SecretString};
use gradient_wire::messages::{CredentialKind, GitServerSshHostKey};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct CredentialStore {
    job_secrets: BTreeMap<(String, String), SecretBytes>,
}

impl CredentialStore {
    pub fn store(&mut self, kind: CredentialKind, data: SecretBytes) {
        let CredentialKind::JobSecret { job_id, name } = kind;
        self.job_secrets.insert((job_id, name), data);
    }

    pub fn take_credentials_for_job(
        &mut self,
        job_id: &str,
        ssh_key: Option<SecretString>,
        ssh_host_keys: Vec<GitServerSshHostKey>,
        https_git_credentials: Vec<HttpsGitCredential>,
    ) -> JobCredentials {
        let (own, others) = std::mem::take(&mut self.job_secrets)
            .into_iter()
            .partition(|((owner, _), _)| owner == job_id);
        self.job_secrets = others;
        JobCredentials {
            ssh_key,
            ssh_host_keys,
            https_git_credentials,
            secrets: own
                .into_iter()
                .map(|((_, name), value)| (name, value))
                .collect(),
        }
    }
}

#[derive(Clone, Default)]
pub struct JobCredentials {
    ssh_key: Option<SecretString>,
    ssh_host_keys: Vec<GitServerSshHostKey>,
    https_git_credentials: Vec<HttpsGitCredential>,
    secrets: BTreeMap<String, SecretBytes>,
}

impl JobCredentials {
    pub fn git_credentials_for_clone_and_fetch(&self) -> GitOperationCredentials {
        GitOperationCredentials {
            ssh_git_private_key_and_known_hosts: self
                .ssh_git_private_key_known_hosts_and_observations(),
            https_git_credentials: self.https_git_credentials.clone(),
        }
    }

    fn ssh_git_private_key_known_hosts_and_observations(
        &self,
    ) -> Option<SshGitPrivateKeyKnownHostsAndObservations> {
        Some(SshGitPrivateKeyKnownHostsAndObservations::new(
            self.ssh_key.clone()?,
            self.ssh_host_keys.clone(),
        ))
    }

    pub fn secret_value_by_name(&self, name: &str) -> Option<SecretBytes> {
        self.secrets.get(name).cloned()
    }

    #[cfg(test)]
    pub(crate) fn ssh_private_key_text(&self) -> Option<&str> {
        self.ssh_key.as_ref().map(SecretString::expose)
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

    fn ssh_host_keys_with_one_ed25519_key_for_host(host: &str) -> Vec<GitServerSshHostKey> {
        vec![GitServerSshHostKey {
            host: host.into(),
            port: 22,
            public_key: "ssh-ed25519 AAAA".into(),
        }]
    }

    fn take_credentials_for_job_without_ssh_key_or_host_keys(
        store: &mut CredentialStore,
        job_id: &str,
    ) -> JobCredentials {
        store.take_credentials_for_job(job_id, None, Vec::new(), Vec::new())
    }

    #[test]
    fn test_job_assignment_ssh_key_and_host_keys_build_git_ssh_setup_for_protocol_66_or_later() {
        let mut store = CredentialStore::default();

        let held = store.take_credentials_for_job(
            "exec:1",
            Some("project-a".into()),
            ssh_host_keys_with_one_ed25519_key_for_host("git.a"),
            Vec::new(),
        );

        assert_eq!(held.ssh_private_key_text(), Some("project-a"));
        assert_eq!(
            held.ssh_host_keys,
            ssh_host_keys_with_one_ed25519_key_for_host("git.a")
        );
        assert!(
            held.ssh_git_private_key_known_hosts_and_observations()
                .is_some()
        );
    }

    #[test]
    fn three_jobs_get_only_their_own_project_keys() {
        let mut store = CredentialStore::default();

        let first = store.take_credentials_for_job(
            "exec:1",
            Some("project-a".into()),
            ssh_host_keys_with_one_ed25519_key_for_host("git.a"),
            Vec::new(),
        );
        let second = take_credentials_for_job_without_ssh_key_or_host_keys(&mut store, "exec:2");
        let third = store.take_credentials_for_job(
            "exec:3",
            Some("project-b".into()),
            ssh_host_keys_with_one_ed25519_key_for_host("git.b"),
            Vec::new(),
        );

        assert_eq!(first.ssh_private_key_text(), Some("project-a"));
        assert_eq!(second.ssh_private_key_text(), None);
        assert!(second.ssh_host_keys.is_empty());
        assert_eq!(third.ssh_private_key_text(), Some("project-b"));
        assert_eq!(
            third.ssh_host_keys,
            ssh_host_keys_with_one_ed25519_key_for_host("git.b")
        );
    }

    #[test]
    fn a_job_keeps_its_credentials_after_the_store_moves_on() {
        let mut store = CredentialStore::default();
        store.store(job_secret("exec:1", "wandb-token"), b"v1".to_vec().into());

        let held =
            store.take_credentials_for_job("exec:1", Some("p1".into()), Vec::new(), Vec::new());
        store.store(job_secret("exec:1", "wandb-token"), b"v2".to_vec().into());

        assert_eq!(held.ssh_private_key_text(), Some("p1"));
        assert_eq!(
            held.secret_value_by_name("wandb-token")
                .expect("kept")
                .expose(),
            b"v1"
        );
    }

    #[test]
    fn a_later_job_without_https_credentials_gets_none_of_the_previous_job() {
        let mut store = CredentialStore::default();

        let first = store
            .take_credentials_for_job(
                "eval:1",
                None,
                Vec::new(),
                vec![HttpsGitCredential {
                    host: "github.com".into(),
                    username: "x-access-token".into(),
                    password: "ghp-secret".into(),
                }],
            )
            .git_credentials_for_clone_and_fetch();
        let second = take_credentials_for_job_without_ssh_key_or_host_keys(&mut store, "eval:2")
            .git_credentials_for_clone_and_fetch();

        let [held] = first.https_git_credentials.as_slice() else {
            panic!("the first job holds exactly its credential");
        };
        assert_eq!(
            (
                held.host.as_str(),
                held.username.as_str(),
                held.password.expose()
            ),
            ("github.com", "x-access-token", "ghp-secret")
        );
        assert!(second.https_git_credentials.is_empty());
    }

    #[test]
    fn taking_one_job_leaves_the_secrets_of_another_job() {
        let mut store = CredentialStore::default();
        store.store(job_secret("exec:1", "wandb-token"), b"v1".to_vec().into());
        store.store(job_secret("exec:2", "wandb-token"), b"v2".to_vec().into());

        let first = take_credentials_for_job_without_ssh_key_or_host_keys(&mut store, "exec:1");
        let second = take_credentials_for_job_without_ssh_key_or_host_keys(&mut store, "exec:2");

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
