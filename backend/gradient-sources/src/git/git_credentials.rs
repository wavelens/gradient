/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_network_limits::{
    BlockingGitFetchOperationLimits, CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS,
    set_libgit2_connect_and_read_timeouts_once,
};
use super::ssh::{SshGitPrivateKeyKnownHostsAndObservations, register};
pub use gradient_wire::types::HttpsGitCredential;
use std::cell::RefCell;
use std::thread::LocalKey;

thread_local! {
    pub(super) static CURRENT_THREAD_GIT_CREDENTIALS: RefCell<GitOperationCredentials> =
        RefCell::new(GitOperationCredentials::default());
}

#[derive(Clone, Debug, Default)]
pub struct GitOperationCredentials {
    pub ssh_git_private_key_and_known_hosts: Option<SshGitPrivateKeyKnownHostsAndObservations>,
    pub https_git_credentials: Vec<HttpsGitCredential>,
}

impl From<Option<SshGitPrivateKeyKnownHostsAndObservations>> for GitOperationCredentials {
    fn from(ssh: Option<SshGitPrivateKeyKnownHostsAndObservations>) -> Self {
        Self {
            ssh_git_private_key_and_known_hosts: ssh,
            https_git_credentials: Vec::new(),
        }
    }
}

pub(crate) fn run_blocking_libgit2_operation_with_git_credentials<T>(
    credentials: &GitOperationCredentials,
    limits: BlockingGitFetchOperationLimits,
    operation: impl FnOnce() -> T,
) -> T {
    register();
    set_libgit2_connect_and_read_timeouts_once();
    let previous = CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS.replace(Some(limits));
    let _restore = Restore(&CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS, previous);
    run_with_replaced_thread_local_git_credentials(credentials.clone(), operation)
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
