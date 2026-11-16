/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_network_limits::set_libgit2_connect_and_read_timeouts_once;
use super::ssh::{SshGitPrivateKeyKnownHostsAndObservations, register};
pub use gradient_wire::types::HttpsGitCredential;
use std::cell::RefCell;

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
    operation: impl FnOnce() -> T,
) -> T {
    register();
    set_libgit2_connect_and_read_timeouts_once();
    let previous = CURRENT_THREAD_GIT_CREDENTIALS.replace(credentials.clone());
    let _restore = Restore(previous);
    operation()
}

struct Restore(GitOperationCredentials);

impl Drop for Restore {
    fn drop(&mut self) {
        CURRENT_THREAD_GIT_CREDENTIALS.set(std::mem::take(&mut self.0));
    }
}
