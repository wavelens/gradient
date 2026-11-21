/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_network_limits::{
    BlockingGitFetchOperationLimits, CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS,
};
use super::git_transport_registration::register_git_transports_once;
use super::guarded_git_network_connections::clone_git_http_client_or_public_client;
use super::ssh::SshGitPrivateKeyKnownHostsAndObservations;
use crate::SourceError;
use gradient_util::http::HttpClientForUserSuppliedUrls;
pub use gradient_wire::types::HttpsGitCredential;
use std::cell::RefCell;
use std::thread::LocalKey;

thread_local! {
    pub(super) static CURRENT_THREAD_GIT_CREDENTIALS: RefCell<GitOperationCredentials> =
        RefCell::new(GitOperationCredentials::default());
    pub(super) static CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT: RefCell<Option<String>> =
        const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub struct GitOperationCredentials {
    pub ssh_git_private_key_and_known_hosts: Option<SshGitPrivateKeyKnownHostsAndObservations>,
    pub https_git_credentials: Vec<HttpsGitCredential>,
    allowlisted_http_client: HttpClientForUserSuppliedUrls,
}

impl Default for GitOperationCredentials {
    fn default() -> Self {
        Self::from_ssh_key_known_hosts_and_https_credentials(None, Vec::new())
    }
}

impl From<Option<SshGitPrivateKeyKnownHostsAndObservations>> for GitOperationCredentials {
    fn from(ssh: Option<SshGitPrivateKeyKnownHostsAndObservations>) -> Self {
        Self::from_ssh_key_known_hosts_and_https_credentials(ssh, Vec::new())
    }
}

impl GitOperationCredentials {
    pub fn from_ssh_key_known_hosts_and_https_credentials(
        ssh_git_private_key_and_known_hosts: Option<SshGitPrivateKeyKnownHostsAndObservations>,
        https_git_credentials: Vec<HttpsGitCredential>,
    ) -> Self {
        Self {
            ssh_git_private_key_and_known_hosts,
            https_git_credentials,
            allowlisted_http_client: clone_git_http_client_or_public_client(),
        }
    }

    pub fn with_guarded_http_client(mut self, client: HttpClientForUserSuppliedUrls) -> Self {
        self.allowlisted_http_client = client;
        self
    }

    pub fn http_client(&self) -> &HttpClientForUserSuppliedUrls {
        &self.allowlisted_http_client
    }
}

pub(crate) fn run_blocking_libgit2_operation_with_git_credentials<T, E: From<SourceError>>(
    credentials: &GitOperationCredentials,
    limits: BlockingGitFetchOperationLimits,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    register_git_transports_once();
    let previous = CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS.replace(Some(limits));
    let _restore = Restore(&CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS, previous);
    let previous_refusal = CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT.take();
    let _restore_refusal = Restore(&CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT, previous_refusal);
    let result = run_with_replaced_thread_local_git_credentials(credentials.clone(), operation);
    match (result, CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT.take()) {
        (Err(_), Some(reason)) => {
            Err(SourceError::GitServerAddressRefusedByAdminAllowlist { reason }.into())
        }
        (result, _) => result,
    }
}

pub(super) fn run_with_replaced_thread_local_git_credentials<T>(
    credentials: GitOperationCredentials,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = CURRENT_THREAD_GIT_CREDENTIALS.replace(credentials);
    let _restore = Restore(&CURRENT_THREAD_GIT_CREDENTIALS, previous);
    operation()
}

struct Restore<T: Default + 'static>(&'static LocalKey<RefCell<T>>, T);

impl<T: Default + 'static> Drop for Restore<T> {
    fn drop(&mut self) {
        self.0.set(std::mem::take(&mut self.1));
    }
}
