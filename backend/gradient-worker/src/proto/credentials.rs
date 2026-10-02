/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Credentials are intentionally NOT persisted to disk and are dropped when the
//! connection is closing. [`SecretBytes`] is locking its pages with `mlock(2)` and zeroing
//! them on drop.

use gradient_types::SecretBytes;
use gradient_util::sync::Mutex;
use gradient_wire::messages::CredentialKind;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    ssh_key: Option<SecretBytes>,
}

#[derive(Clone, Default)]
pub struct CredentialStore {
    inner: Arc<Mutex<Inner>>,
}

impl CredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&self, kind: CredentialKind, data: Vec<u8>) {
        let mut inner = self.inner.lock();
        match kind {
            CredentialKind::SshKey => {
                inner.ssh_key = Some(SecretBytes::new(data));
            }
        }
    }

    pub fn ssh_key(&self) -> Option<SecretBytes> {
        self.inner
            .lock()
            .ssh_key
            .as_ref()
            .map(|b| SecretBytes::new(b.expose().to_vec()))
    }

    pub fn snapshot(&self) -> Self {
        let store = Self::new();
        if let Some(key) = self.ssh_key() {
            store.store(CredentialKind::SshKey, key.expose().to_vec());
        }
        store
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.ssh_key = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_keeps_its_key_after_the_store_moves_on() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SshKey, b"p1".to_vec());

        let held = store.snapshot();
        store.clear();
        store.store(CredentialKind::SshKey, b"p2".to_vec());

        assert_eq!(held.ssh_key().expect("kept").expose(), b"p1");
    }
}
