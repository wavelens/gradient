/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use gradient_db::DbContext;
use tracing::warn;

use super::StorageMigration;

const FLAT_KEYS: &str = "flat-keys";

pub(super) struct EvalCachePerCache;

impl StorageMigration for EvalCachePerCache {
    fn name(&self) -> &'static str {
        "m20261019_000000_eval_cache_per_cache"
    }

    fn units(&self) -> Vec<String> {
        vec![FLAT_KEYS.to_owned()]
    }

    fn migrate<'a>(&'a self, db: &'a DbContext, _unit: &'a str) -> BoxFuture<'a, Result<()>> {
        delete_flat_keys(db).boxed()
    }
}

fn is_flat(key: &str) -> bool {
    key.strip_prefix("eval-cache/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

async fn delete_flat_keys(db: &DbContext) -> Result<()> {
    let store = &db.storage.nar_storage;
    for key in store.list_eval_cache_keys().await? {
        if is_flat(&key)
            && let Err(e) = store.delete_eval_cache(&key).await
        {
            warn!(%key, error = %e, "flat eval-cache blob delete failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_fixtures::mock::empty_db;
    use gradient_storage::NarStore;
    use gradient_types::ids::CacheId;

    #[tokio::test]
    async fn only_eval_cache_blobs_without_a_cache_are_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path()).unwrap();
        let per_cache = format!("eval-cache/{}/fp2", CacheId::now_v7());
        nar.put_eval_cache("eval-cache/fp1", b"flat".to_vec())
            .await
            .unwrap();
        nar.put_eval_cache(&per_cache, b"kept".to_vec())
            .await
            .unwrap();
        let state = test_server_state(nar, empty_db(), |_| {});

        EvalCachePerCache
            .migrate(&state.db(), "flat-keys")
            .await
            .unwrap();

        let store = &state.db().storage.nar_storage;
        assert_eq!(store.get_eval_cache("eval-cache/fp1").await.unwrap(), None);
        assert!(store.get_eval_cache(&per_cache).await.unwrap().is_some());
    }
}
