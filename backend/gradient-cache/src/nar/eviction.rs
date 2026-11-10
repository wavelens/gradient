/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_storage::NarStore;
use std::collections::HashSet;
use tracing::warn;

#[async_trait::async_trait]
pub trait CachedPathIndex: Send + Sync {
    async fn expired(&self) -> anyhow::Result<Vec<String>>;
    async fn retire(&self, keys: &[String]) -> anyhow::Result<Vec<String>>;
    async fn filter_nar_hashes_with_cached_path_again(
        &self,
        keys: &[String],
    ) -> anyhow::Result<HashSet<String>>;
}

pub async fn evict(
    index: &dyn CachedPathIndex,
    store: &NarStore,
    chunk: usize,
) -> anyhow::Result<u64> {
    let expired = index.expired().await?;
    let mut evicted = 0u64;
    for keys in expired.chunks(chunk.max(1)) {
        let retired = index.retire(keys).await?;
        let stored_again = index
            .filter_nar_hashes_with_cached_path_again(&retired)
            .await?;
        for key in retired {
            evicted += 1;
            if stored_again.contains(&key) {
                continue;
            }
            if let Err(e) = store.delete(&key).await {
                warn!(error = %e, %key, "failed to remove an evicted NAR");
            }
        }
    }
    Ok(evicted)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gradient_storage::NarStore;
    use tempfile::TempDir;

    use super::*;

    struct Index {
        expired: Vec<String>,
        kept: HashSet<String>,
        uploaded_again: HashSet<String>,
        chunks: Mutex<Vec<usize>>,
    }

    #[async_trait::async_trait]
    impl CachedPathIndex for Index {
        async fn expired(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.expired.clone())
        }

        async fn retire(&self, keys: &[String]) -> anyhow::Result<Vec<String>> {
            self.chunks.lock().expect("lock").push(keys.len());
            Ok(keys
                .iter()
                .filter(|k| !self.kept.contains(*k))
                .cloned()
                .collect())
        }

        async fn filter_nar_hashes_with_cached_path_again(
            &self,
            keys: &[String],
        ) -> anyhow::Result<HashSet<String>> {
            Ok(keys
                .iter()
                .filter(|k| self.uploaded_again.contains(*k))
                .cloned()
                .collect())
        }
    }

    async fn store_with(keys: &[&str]) -> (TempDir, NarStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = NarStore::local(dir.path()).expect("store");
        for key in keys {
            store.put(key, b"nar".to_vec()).await.expect("put");
        }
        (dir, store)
    }

    #[tokio::test]
    async fn retired_keys_leave_storage_and_refused_keys_stay() {
        let (_dir, store) = store_with(&["aa1", "bb2", "cc3"]).await;
        let index = Index {
            expired: vec!["aa1".into(), "bb2".into(), "cc3".into()],
            kept: HashSet::from(["bb2".to_owned()]),
            uploaded_again: HashSet::new(),
            chunks: Mutex::default(),
        };

        assert_eq!(evict(&index, &store, 2).await.expect("evict"), 2);
        assert!(!store.exists("aa1").await.expect("exists"));
        assert!(store.exists("bb2").await.expect("exists"));
        assert!(!store.exists("cc3").await.expect("exists"));
        assert_eq!(*index.chunks.lock().expect("lock"), vec![2, 1]);
    }

    #[tokio::test]
    async fn a_path_uploaded_again_after_retiring_keeps_its_object() {
        let (_dir, store) = store_with(&["aa1", "bb2"]).await;
        let index = Index {
            expired: vec!["aa1".into(), "bb2".into()],
            kept: HashSet::new(),
            uploaded_again: HashSet::from(["bb2".to_owned()]),
            chunks: Mutex::default(),
        };

        assert_eq!(evict(&index, &store, 10).await.expect("evict"), 2);
        assert!(!store.exists("aa1").await.expect("exists"));
        assert!(store.exists("bb2").await.expect("exists"));
    }

    #[tokio::test]
    async fn nothing_expired_touches_nothing() {
        let (_dir, store) = store_with(&["aa1"]).await;
        let index = Index {
            expired: Vec::new(),
            kept: HashSet::new(),
            uploaded_again: HashSet::new(),
            chunks: Mutex::default(),
        };

        assert_eq!(evict(&index, &store, 10).await.expect("evict"), 0);
        assert!(index.chunks.lock().expect("lock").is_empty());
        assert!(store.exists("aa1").await.expect("exists"));
    }
}
