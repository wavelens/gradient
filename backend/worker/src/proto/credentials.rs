/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Short-lived credential storage for the worker.
//!
//! The server delivers credentials (SSH keys, signing keys) via
//! [`ServerMessage::Credential`] just before or alongside [`ServerMessage::AssignJob`].
//! The worker stores the most-recently-received credential of each kind and
//! makes it available to executors that need it.
//!
//! Credentials are intentionally NOT persisted to disk and are dropped when the
//! connection closes. Both [`SecretString`] and [`SecretBytes`] lock their
//! memory pages with `mlock(2)` and zero them on drop.

use gradient_core::types::{SecretBytes, SecretString};
use proto::messages::CredentialKind;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    signing_key: Option<SecretString>,
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
            CredentialKind::SigningKey => {
                inner.signing_key = String::from_utf8(data).ok().map(SecretString::new);
            }
            CredentialKind::SshKey => {
                inner.ssh_key = Some(SecretBytes::new(data));
            }
        }
    }

    /// Retrieve the signing key (Ed25519 `name:base64` format).
    /// Returns a clone of the secret — the caller is responsible for dropping
    /// it promptly after use.
    pub fn signing_key(&self) -> Option<SecretString> {
        self.inner
            .lock()
            .unwrap()
            .signing_key
            .as_ref()
            .map(|s| SecretString::new(s.expose().to_string()))
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
        inner.signing_key = None;
        inner.ssh_key = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_signing_key() {
        let store = CredentialStore::new();
        store.store(
            CredentialKind::SigningKey,
            b"cache.example.com:AAAA".to_vec(),
        );
        let key = store.signing_key().expect("signing key should be present");
        assert_eq!(key.expose(), "cache.example.com:AAAA");
    }

    #[test]
    fn store_and_retrieve_ssh_key() {
        let store = CredentialStore::new();
        let raw = vec![0xDE, 0xAD, 0xBE, 0xEF];
        store.store(CredentialKind::SshKey, raw.clone());
        let key = store.ssh_key().expect("ssh key should be present");
        assert_eq!(key.expose(), raw.as_slice());
    }

    #[test]
    fn signing_key_invalid_utf8_stores_none() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SigningKey, vec![0xFF, 0xFE]);
        assert!(store.signing_key().is_none());
    }

    #[test]
    fn clear_drops_both() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SigningKey, b"key".to_vec());
        store.store(CredentialKind::SshKey, vec![1, 2, 3]);
        store.clear();
        assert!(store.signing_key().is_none());
        assert!(store.ssh_key().is_none());
    }

    #[test]
    fn overwrite_replaces_previous() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SigningKey, b"key-a".to_vec());
        store.store(CredentialKind::SigningKey, b"key-b".to_vec());
        let key = store.signing_key().unwrap();
        assert_eq!(key.expose(), "key-b");
    }

    #[test]
    fn clone_shares_state() {
        let store = CredentialStore::new();
        let clone = store.clone();
        // Store via clone, retrieve via original.
        clone.store(CredentialKind::SigningKey, b"shared".to_vec());
        let key = store
            .signing_key()
            .expect("original should see cloned value");
        assert_eq!(key.expose(), "shared");
    }
}
