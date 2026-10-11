/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use gradient_types::ids::UserId;
use rand::RngExt as _;

use crate::github_app_manifest::ManifestResult;

pub const STATE_TTL: Duration = Duration::from_secs(10 * 60);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// The user id is recorded at issuance because the callback is an unauthenticated browser redirect
/// from github.com. The callback can then recover the initiating superuser without trusting
/// query-string input.
#[derive(Clone, Debug, Default)]
pub struct ManifestStateStore(Arc<Mutex<HashMap<String, (UserId, Instant)>>>);

impl ManifestStateStore {
    pub async fn issue_state(&self, user_id: UserId) -> String {
        let mut bytes = [0u8; 24];
        rand::rng().fill(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);

        let mut guard = lock(&self.0);
        let cutoff = Instant::now() - STATE_TTL;
        guard.retain(|_, (_, ts)| *ts > cutoff);
        guard.insert(token.clone(), (user_id, Instant::now()));
        token
    }

    pub async fn validate_and_consume(&self, state: &str) -> Option<UserId> {
        match lock(&self.0).remove(state) {
            Some((user_id, ts)) if ts > Instant::now() - STATE_TTL => Some(user_id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PendingCredentialsStore(Arc<Mutex<HashMap<UserId, (ManifestResult, Instant)>>>);

impl PendingCredentialsStore {
    pub async fn store_credentials(&self, user_id: UserId, creds: ManifestResult) {
        let mut guard = lock(&self.0);
        let cutoff = Instant::now() - STATE_TTL;
        guard.retain(|_, (_, ts)| *ts > cutoff);
        guard.insert(user_id, (creds, Instant::now()));
    }

    pub async fn take_credentials(&self, user_id: UserId) -> Option<ManifestResult> {
        match lock(&self.0).remove(&user_id) {
            Some((creds, ts)) if ts > Instant::now() - STATE_TTL => Some(creds),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn issue_state_returns_unique_tokens() {
        let store = ManifestStateStore::default();
        let a = store.issue_state(UserId::now_v7()).await;
        let b = store.issue_state(UserId::now_v7()).await;
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }

    #[tokio::test]
    async fn validate_and_consume_returns_user_then_fails_on_replay() {
        let store = ManifestStateStore::default();
        let user = UserId::now_v7();
        let s = store.issue_state(user).await;
        assert_eq!(store.validate_and_consume(&s).await, Some(user));
        assert_eq!(store.validate_and_consume(&s).await, None);
    }

    #[tokio::test]
    async fn validate_and_consume_unknown_state_fails() {
        let store = ManifestStateStore::default();
        assert_eq!(store.validate_and_consume("not-a-real-state").await, None);
    }

    #[tokio::test]
    async fn issue_state_prunes_expired_entries() {
        let store = ManifestStateStore::default();
        let expired = "expired-token".to_string();
        lock(&store.0).insert(
            expired.clone(),
            (
                UserId::now_v7(),
                Instant::now() - Duration::from_secs(11 * 60),
            ),
        );
        let _fresh = store.issue_state(UserId::now_v7()).await;
        assert!(!lock(&store.0).contains_key(&expired));
    }

    #[tokio::test]
    async fn store_and_take_credentials_one_shot() {
        let store = PendingCredentialsStore::default();
        let user = UserId::now_v7();
        let creds = ManifestResult {
            id: 1,
            slug: "x".into(),
            html_url: "https://github.com/apps/x".into(),
            pem: "PEM".into(),
            webhook_secret: "ws".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
        };
        store.store_credentials(user, creds).await;
        let taken = store
            .take_credentials(user)
            .await
            .expect("first read returns creds");
        assert_eq!(taken.id, 1);
        assert!(store.take_credentials(user).await.is_none());
    }
}
