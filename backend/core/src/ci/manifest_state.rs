/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory short-lived stores for the GitHub App manifest flow:
//! - CSRF state tokens issued at /admin/github-app/manifest and consumed at
//!   /admin/github-app/callback.
//! - Pending credential blobs stored after a successful exchange and consumed
//!   by the operator's browser session at /admin/github-app/credentials.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use uuid::Uuid;

use crate::ci::github_app_manifest::ManifestResult;

/// Map of state-token → (initiating superuser id, issuance time). Tokens older
/// than 10 minutes are pruned on each `issue_state` call.
///
/// The user id is recorded at issuance so the callback — which arrives as an
/// unauthenticated top-level browser redirect from github.com and therefore
/// carries no `Authorization` header — can recover which superuser initiated
/// the manifest flow without trusting query-string input.
pub type ManifestStateStore = Mutex<HashMap<String, (Uuid, Instant)>>;

/// Map of superuser id → (pending credentials, deposit time). Entries older
/// than 10 minutes are pruned on each `store_credentials` call.
pub type PendingCredentialsStore = Mutex<HashMap<Uuid, (ManifestResult, Instant)>>;

use rand::RngExt as _;
use std::time::Duration;

/// State tokens older than this are pruned and become invalid.
pub const STATE_TTL: Duration = Duration::from_secs(10 * 60);

/// Generates and stores a fresh URL-safe random state token. Prunes any
/// expired entries as a side-effect.
pub fn issue_state(store: &ManifestStateStore, user_id: Uuid) -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill(&mut bytes);
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let token = URL_SAFE_NO_PAD.encode(bytes);

    let mut guard = store.lock().expect("manifest state store poisoned");
    let cutoff = Instant::now() - STATE_TTL;
    guard.retain(|_, (_, ts)| *ts > cutoff);
    guard.insert(token.clone(), (user_id, Instant::now()));
    token
}

/// Removes the state from the store and returns the initiating user id iff
/// the token existed and is not expired. One-shot consumption.
pub fn validate_and_consume(store: &ManifestStateStore, state: &str) -> Option<Uuid> {
    let mut guard = store.lock().expect("manifest state store poisoned");
    match guard.remove(state) {
        Some((user_id, ts)) if ts > Instant::now() - STATE_TTL => Some(user_id),
        _ => None,
    }
}

/// Stores `creds` keyed by `user_id`, overwriting any prior entry. Prunes
/// expired entries as a side-effect.
pub fn store_credentials(
    store: &PendingCredentialsStore,
    user_id: Uuid,
    creds: ManifestResult,
) {
    let mut guard = store.lock().expect("pending credentials store poisoned");
    let cutoff = Instant::now() - STATE_TTL;
    guard.retain(|_, (_, ts)| *ts > cutoff);
    guard.insert(user_id, (creds, Instant::now()));
}

/// Removes and returns the entry for `user_id` if present and not expired.
pub fn take_credentials(
    store: &PendingCredentialsStore,
    user_id: Uuid,
) -> Option<ManifestResult> {
    let mut guard = store.lock().expect("pending credentials store poisoned");
    match guard.remove(&user_id) {
        Some((creds, ts)) if ts > Instant::now() - STATE_TTL => Some(creds),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    fn empty_state_store() -> ManifestStateStore {
        Mutex::new(HashMap::new())
    }

    #[test]
    fn issue_state_returns_unique_tokens() {
        let store = empty_state_store();
        let a = issue_state(&store, Uuid::now_v7());
        let b = issue_state(&store, Uuid::now_v7());
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }

    #[test]
    fn validate_and_consume_returns_user_then_fails_on_replay() {
        let store = empty_state_store();
        let user = Uuid::now_v7();
        let s = issue_state(&store, user);
        assert_eq!(validate_and_consume(&store, &s), Some(user));
        assert_eq!(validate_and_consume(&store, &s), None);
    }

    #[test]
    fn validate_and_consume_unknown_state_fails() {
        let store = empty_state_store();
        assert_eq!(validate_and_consume(&store, "not-a-real-state"), None);
    }

    #[test]
    fn issue_state_prunes_expired_entries() {
        let store = empty_state_store();
        let stale = "stale-token".to_string();
        store.lock().unwrap().insert(
            stale.clone(),
            (Uuid::now_v7(), Instant::now() - Duration::from_secs(11 * 60)),
        );
        let _fresh = issue_state(&store, Uuid::now_v7());
        assert!(!store.lock().unwrap().contains_key(&stale));
    }

    #[test]
    fn store_and_take_credentials_one_shot() {
        let store: PendingCredentialsStore = Mutex::new(HashMap::new());
        let user = Uuid::now_v7();
        let creds = ManifestResult {
            id: 1,
            slug: "x".into(),
            html_url: "https://github.com/apps/x".into(),
            pem: "PEM".into(),
            webhook_secret: "ws".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
        };
        store_credentials(&store, user, creds);
        let taken = take_credentials(&store, user).expect("first read returns creds");
        assert_eq!(taken.id, 1);
        assert!(take_credentials(&store, user).is_none());
    }
}
