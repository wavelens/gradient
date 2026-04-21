/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Short-lived credential storage for the worker.
//!
//! The server delivers credentials (SSH keys) via
//! [`ServerMessage::Credential`] just before or alongside [`ServerMessage::AssignJob`].
//! The worker stores the most-recently-received credential of each kind and
//! makes it available to executors that need it.
//!
//! Credentials are intentionally NOT persisted to disk and are dropped when the
//! connection closes. [`SecretBytes`] locks its memory pages with `mlock(2)`
//! and zeros it on drop.

use gradient_core::types::SecretBytes;
use proto::messages::CredentialKind;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    ssh_key: Option<SecretBytes>,
}

/// Thread-safe, in-memory credential store.
#[derive(Clone, Default)]
pub struct CredentialStore {
    inner: Arc<Mutex<Inner>>,
}

impl CredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a credential delivered by the server.
    pub fn store(&self, kind: CredentialKind, data: Vec<u8>) {
        let mut inner = self.inner.lock().unwrap();
        match kind {
            CredentialKind::SshKey => {
                inner.ssh_key = Some(SecretBytes::new(data));
            }
        }
    }

    /// Retrieve the SSH private key bytes.
    pub fn ssh_key(&self) -> Option<SecretBytes> {
        self.inner
            .lock()
            .unwrap()
            .ssh_key
            .as_ref()
            .map(|b| SecretBytes::new(b.expose().to_vec()))
    }

    /// Clear all stored credentials (called after a job completes).
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.ssh_key = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_ssh_key() {
        let store = CredentialStore::new();
        let raw = vec![0xDE, 0xAD, 0xBE, 0xEF];
        store.store(CredentialKind::SshKey, raw.clone());
        let key = store.ssh_key().expect("ssh key should be present");
        assert_eq!(key.expose(), raw.as_slice());
    }

    #[test]
    fn clear_drops_ssh() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SshKey, vec![1, 2, 3]);
        store.clear();
        assert!(store.ssh_key().is_none());
    }

    #[test]
    fn clone_shares_state() {
        let store = CredentialStore::new();
        let clone = store.clone();
        clone.store(CredentialKind::SshKey, vec![0xAA]);
        let key = store.ssh_key().expect("ssh key should be present");
        assert_eq!(key.expose(), &[0xAA]);
    }
}
