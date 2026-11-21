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

use anyhow::Context as _;
use gradient_sources::{
    GitOperationCredentials, HttpsGitCredential, SshGitPrivateKeyKnownHostsAndObservations,
};
use gradient_types::{SecretBytes, SecretString};
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::http_validation::UserUrlDestinationAllowlist;
use gradient_wire::messages::{CredentialKind, GitServerSshHostKey};
use std::collections::BTreeMap;

pub fn parse_allowlist_entries_and_build_http_client(
    entries: &[String],
) -> anyhow::Result<HttpClientForUserSuppliedUrls> {
    UserUrlDestinationAllowlist::parse(entries)
        .map_err(anyhow::Error::from)
        .and_then(|allowlist| Ok(HttpClientForUserSuppliedUrls::new(allowlist)?))
        .context(
            "Build the HTTP client for Git connections from the allowlist that the server sent.",
        )
}

#[derive(Default)]
pub struct CredentialStore {
    job_secrets: BTreeMap<(String, String), SecretBytes>,
    http_client_for_git_urls_limited_to_allowed_destinations: Option<HttpClientForUserSuppliedUrls>,
}

impl CredentialStore {
    pub fn new_with_git_connection_http_client(client: HttpClientForUserSuppliedUrls) -> Self {
        Self {
            job_secrets: BTreeMap::new(),
            http_client_for_git_urls_limited_to_allowed_destinations: Some(client),
        }
    }

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
            http_client_for_git_urls_limited_to_allowed_destinations: self
                .http_client_for_git_urls_limited_to_allowed_destinations
                .clone(),
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
    http_client_for_git_urls_limited_to_allowed_destinations: Option<HttpClientForUserSuppliedUrls>,
    secrets: BTreeMap<String, SecretBytes>,
}

impl JobCredentials {
    pub fn git_credentials_for_clone_and_fetch(&self) -> GitOperationCredentials {
        let credentials = GitOperationCredentials::from_ssh_key_known_hosts_and_https_credentials(
            self.ssh_git_private_key_known_hosts_and_observations(),
            self.https_git_credentials.clone(),
        );
        match &self.http_client_for_git_urls_limited_to_allowed_destinations {
            Some(client) => credentials.with_guarded_http_client(client.clone()),
            None => credentials,
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
    use gradient_sources::SourceError;
    use gradient_types::RepositoryUrl;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn bind_loopback_tcp_listener_and_count_accepted_connections() -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        (port, accepted)
    }

    fn checkout_git_repo_with_job_context_from_credential_store_allowlist_and_return_error(
        port: u16,
        allowlist_entries: &[String],
    ) -> anyhow::Error {
        let client = parse_allowlist_entries_and_build_http_client(allowlist_entries).unwrap();
        let mut store = CredentialStore::new_with_git_connection_http_client(client);
        let context = take_credentials_for_job_without_ssh_key_or_host_keys(&mut store, "exec:1")
            .git_credentials_for_clone_and_fetch();
        let url: RepositoryUrl = format!("git://127.0.0.1:{port}/repo").parse().unwrap();
        let work = tempfile::tempdir().unwrap();
        gradient_sources::checkout_commit_into(work.path(), &url, &"a".repeat(40), &context)
            .unwrap_err()
    }

    fn wait_until_connection_count_reaches_expected(
        accepted: &AtomicUsize,
        expected: usize,
    ) -> usize {
        for _ in 0..50 {
            if accepted.load(Ordering::SeqCst) >= expected {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        accepted.load(Ordering::SeqCst)
    }

    #[test]
    fn credential_store_git_context_allows_loopback_when_allowlisted_and_refuses_without_tcp_when_empty()
     {
        let (allowed_port, allowed_accepted) =
            bind_loopback_tcp_listener_and_count_accepted_connections();
        checkout_git_repo_with_job_context_from_credential_store_allowlist_and_return_error(
            allowed_port,
            &["127.0.0.0/8".to_owned()],
        );
        assert_eq!(
            wait_until_connection_count_reaches_expected(&allowed_accepted, 1),
            1,
            "127.0.0.0/8 is in the session allowlist. The Git connection can reach the local listener."
        );

        let (refused_port, refused_accepted) =
            bind_loopback_tcp_listener_and_count_accepted_connections();
        let refused =
            checkout_git_repo_with_job_context_from_credential_store_allowlist_and_return_error(
                refused_port,
                &[],
            );
        match refused.downcast_ref::<SourceError>() {
            Some(SourceError::GitServerAddressRefusedByAdminAllowlist { .. }) => {}
            other => panic!("Expected a rejection from the admin allowlist, but got {other:?}"),
        }
        assert_eq!(
            wait_until_connection_count_reaches_expected(&refused_accepted, 1),
            0,
            "The session allowlist is empty, so no Git connection can reach the local listener."
        );
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
