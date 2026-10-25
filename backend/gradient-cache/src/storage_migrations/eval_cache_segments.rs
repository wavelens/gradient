/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use gradient_db::DbContext;
use gradient_storage::NarStore;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use tracing::warn;

use super::StorageMigration;

const WHOLE_BLOBS: &str = "whole-blobs";

const WHOLE_BLOB_PREFIX: &str = "eval-cache/";

pub(super) struct EvalCacheSegments;

impl StorageMigration for EvalCacheSegments {
    fn name(&self) -> &'static str {
        "m20261025_000000_eval_cache_segments"
    }

    fn units(&self) -> Vec<String> {
        vec![WHOLE_BLOBS.to_owned()]
    }

    fn migrate<'a>(&'a self, db: &'a DbContext, _unit: &'a str) -> BoxFuture<'a, Result<()>> {
        drop_whole_blobs(&db.storage.nar_storage, &db.worker_db).boxed()
    }
}

async fn drop_whole_blobs(store: &NarStore, db: &impl ConnectionTrait) -> Result<()> {
    for key in store.list_eval_cache_keys().await? {
        if let Err(e) = store.delete_eval_cache(&key).await {
            warn!(%key, error = %e, "whole eval cache blob delete failed");
        }
    }

    EEvalCacheStore::delete_many()
        .filter(CEvalCacheStore::StoragePath.starts_with(WHOLE_BLOB_PREFIX))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
    use gradient_storage::eval_cache_segment::eval_cache_segment_key;
    use gradient_types::ids::CacheId;

    #[tokio::test]
    async fn old_blobs_and_their_rows_go_segments_stay() {
        let cache = CacheId::now_v7();
        let tmp = tempfile::tempdir().unwrap();
        let store = NarStore::local(tmp.path()).unwrap();
        let blob = format!("eval-cache/{cache}/fp");
        let segment = eval_cache_segment_key(cache, "fp", uuid::Uuid::now_v7());
        store
            .put_eval_cache(&blob, b"whole".to_vec())
            .await
            .unwrap();
        store
            .put_eval_cache(&segment, b"segment".to_vec())
            .await
            .unwrap();
        let db = mock_db().append_exec_results([exec_ok()]).into_connection();

        drop_whole_blobs(&store, &db).await.unwrap();

        assert_eq!(store.get_eval_cache(&blob).await.unwrap(), None);
        assert!(store.get_eval_cache(&segment).await.unwrap().is_some());
        let deleted = &raw_statements(db.into_transaction_log())[0];
        assert!(deleted.sql.starts_with("DELETE FROM"), "{}", deleted.sql);
        assert!(
            format!("{:?}", deleted.values).contains("eval-cache/%"),
            "{:?}",
            deleted.values
        );
    }
}
