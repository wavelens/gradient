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
//! connection closes.

use proto::messages::CredentialKind;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    signing_key: Option<String>,
    ssh_key: Option<Vec<u8>>,
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
                inner.signing_key = String::from_utf8(data).ok();
            }
            CredentialKind::SshKey => {
                inner.ssh_key = Some(data);
            }
        }
    }

    /// Retrieve the signing key (Ed25519 `name:base64` format).
    pub fn signing_key(&self) -> Option<String> {
        self.inner.lock().unwrap().signing_key.clone()
    }

    /// Retrieve the SSH private key bytes.
    pub fn ssh_key(&self) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().ssh_key.clone()
    }

    /// Clear all stored credentials (called after a job completes).
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.signing_key = None;
        inner.ssh_key = None;
    }
}
