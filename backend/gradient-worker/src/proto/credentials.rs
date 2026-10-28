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
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    ssh_key: Option<SecretBytes>,
    job_secrets: BTreeMap<(String, String), SecretBytes>,
}

#[derive(Clone, Default)]
pub struct CredentialStore {
    inner: Arc<Mutex<Inner>>,
}

impl CredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&self, kind: CredentialKind, data: SecretBytes) {
        let mut inner = self.inner.lock();
        match kind {
            CredentialKind::SshKey => {
                inner.ssh_key = Some(data);
            }
            CredentialKind::JobSecret { job_id, name } => {
                inner.job_secrets.insert((job_id, name), data);
            }
        }
    }

    pub fn ssh_key(&self) -> Option<SecretBytes> {
        self.inner.lock().ssh_key.clone()
    }

    pub fn job_secret(&self, job_id: &str, name: &str) -> Option<SecretBytes> {
        self.inner
            .lock()
            .job_secrets
            .get(&(job_id.to_owned(), name.to_owned()))
            .cloned()
    }

    pub fn snapshot(&self) -> Self {
        let inner = self.inner.lock();
        let copied = Inner {
            ssh_key: inner.ssh_key.clone(),
            job_secrets: inner.job_secrets.clone(),
        };
        Self {
            inner: Arc::new(Mutex::new(copied)),
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.ssh_key = None;
        inner.job_secrets.clear();
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

    #[test]
    fn a_snapshot_keeps_its_key_after_the_store_moves_on() {
        let store = CredentialStore::new();
        store.store(CredentialKind::SshKey, b"p1".to_vec().into());

        let held = store.snapshot();
        store.clear();
        store.store(CredentialKind::SshKey, b"p2".to_vec().into());

        assert_eq!(held.ssh_key().expect("kept").expose(), b"p1");
    }

    #[test]
    fn a_snapshot_keeps_the_job_secrets() {
        let store = CredentialStore::new();
        store.store(job_secret("exec:1", "wandb-token"), b"v1".to_vec().into());
        let held = store.snapshot();
        store.clear();
        assert_eq!(
            held.job_secret("exec:1", "wandb-token")
                .expect("kept")
                .expose(),
            b"v1"
        );
    }

    #[test]
    fn job_secrets_of_one_job_stay_out_of_another() {
        let store = CredentialStore::new();
        store.store(job_secret("exec:1", "wandb-token"), b"v1".to_vec().into());
        assert!(store.job_secret("exec:2", "wandb-token").is_none());
    }
}
