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
    /// One signing key per org cache. The server sends one `Credential`
    /// message per cache; the worker accumulates them and signs every
    /// uploaded path once per key.
    signing_keys: Vec<SecretString>,
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

    /// Store a credential delivered by the server. For `SigningKey` the
    /// credential is appended to the current list — each delivery adds one
    /// more cache's signing key.
    pub fn store(&self, kind: CredentialKind, data: Vec<u8>) {
        let mut inner = self.inner.lock().unwrap();
        match kind {
            CredentialKind::SigningKey => {
                if let Some(s) = String::from_utf8(data).ok().map(SecretString::new) {
                    inner.signing_keys.push(s);
                }
            }
            CredentialKind::SshKey => {
                inner.ssh_key = Some(SecretBytes::new(data));
            }
        }
    }

    /// All signing keys delivered so far. Each entry is a Nix signing key
    /// in `"cache-name:base64"` format. Returned clones are independent
    /// copies; drop them promptly.
    pub fn signing_keys(&self) -> Vec<SecretString> {
        self.inner
            .lock()
            .unwrap()
            .signing_keys
            .iter()
            .map(|s| SecretString::new(s.expose().to_string()))
            .collect()
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
        inner.signing_keys.clear();
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
        let keys = store.signing_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].expose(), "cache.example.com:AAAA");
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
    fn signing_key_invalid_utf8_is_dropped() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SigningKey, vec![0xFF, 0xFE]);
        assert!(store.signing_keys().is_empty());
    }

    #[test]
    fn clear_drops_both() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SigningKey, b"key-a:x".to_vec());
        store.store(CredentialKind::SigningKey, b"key-b:y".to_vec());
        store.store(CredentialKind::SshKey, vec![1, 2, 3]);
        store.clear();
        assert!(store.signing_keys().is_empty());
        assert!(store.ssh_key().is_none());
    }

    #[test]
    fn multiple_signing_keys_accumulate() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SigningKey, b"cache-a:AAAA".to_vec());
        store.store(CredentialKind::SigningKey, b"cache-b:BBBB".to_vec());
        let keys = store.signing_keys();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].expose(), "cache-a:AAAA");
        assert_eq!(keys[1].expose(), "cache-b:BBBB");
    }

    #[test]
    fn clone_shares_state() {
        let store = CredentialStore::new();
        let clone = store.clone();
        clone.store(CredentialKind::SigningKey, b"shared:x".to_vec());
        let keys = store.signing_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].expose(), "shared:x");
    }
}
