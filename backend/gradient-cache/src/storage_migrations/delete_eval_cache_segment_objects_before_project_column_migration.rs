/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use gradient_db::DbContext;
use gradient_db::maintenance::storage_migrations::{
    DELETE_EVALUATION_CACHE_SEGMENTS_WRITTEN_BEFORE_UPGRADE_MIGRATION_NAME,
    add_project_to_eval_cache_store_migration_version, migration_applied_at_unix_seconds,
};
use gradient_storage::NarStore;

use super::StorageMigration;

const SEGMENTS_WRITTEN_BEFORE_UPGRADE: &str = "segments-written-before-upgrade";

// The clock of the object store can run ahead of the server clock that gave the database migration its timestamp. This tolerance can move the cutoff later. The sweep can then delete a few new segments rather than leave planted segments.
const OBJECT_STORE_CLOCK_MAX_SECONDS_AHEAD_OF_SERVER_CLOCK: i64 = 15 * 60;

pub(super) struct DeleteEvaluationCacheSegmentObjectsBeforeProjectColumnMigration;

impl StorageMigration for DeleteEvaluationCacheSegmentObjectsBeforeProjectColumnMigration {
    fn name(&self) -> &'static str {
        DELETE_EVALUATION_CACHE_SEGMENTS_WRITTEN_BEFORE_UPGRADE_MIGRATION_NAME
    }

    fn units(&self) -> Vec<String> {
        vec![SEGMENTS_WRITTEN_BEFORE_UPGRADE.to_owned()]
    }

    fn migrate<'a>(&'a self, db: &'a DbContext, _unit: &'a str) -> BoxFuture<'a, Result<()>> {
        async move {
            let version = add_project_to_eval_cache_store_migration_version();
            let applied_at = migration_applied_at_unix_seconds(&db.worker_db, version)
                .await?
                .with_context(|| format!("Database migration {version} has not run"))?;
            delete_eval_cache_segments_modified_before(
                &db.storage.nar_storage,
                applied_at + OBJECT_STORE_CLOCK_MAX_SECONDS_AHEAD_OF_SERVER_CLOCK,
            )
            .await
        }
        .boxed()
    }
}

async fn delete_eval_cache_segments_modified_before(
    store: &NarStore,
    cutoff_unix_seconds: i64,
) -> Result<()> {
    for (key, last_modified) in store
        .list_eval_cache_segment_keys_with_last_modified_unix_seconds()
        .await?
    {
        if last_modified < cutoff_unix_seconds {
            store.delete_eval_cache(&key).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::eval_cache_segment::{
        CheckedEvalCacheFingerprint, eval_cache_segment_key,
    };
    use gradient_types::ids::{CacheId, ProjectId};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    async fn list_eval_cache_segment_object_keys(store: &NarStore) -> Vec<String> {
        let mut keys: Vec<String> = store
            .list_eval_cache_segment_keys_with_last_modified_unix_seconds()
            .await
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        keys.sort_unstable();
        keys
    }

    fn set_last_modified(base: &std::path::Path, key: &str, time: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(base.join(key))
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[tokio::test]
    async fn pre_upgrade_segment_keyed_to_another_project_is_deleted_other_project_listing_is_empty_and_post_upgrade_segment_survives_every_run()
     {
        let tmp = tempfile::tempdir().unwrap();
        let store = NarStore::local(tmp.path()).unwrap();
        let cache = CacheId::now_v7();
        let other_project = ProjectId::now_v7();
        let fingerprint = CheckedEvalCacheFingerprint::parse(&"a".repeat(64)).unwrap();
        let upgrade = SystemTime::now() - Duration::from_secs(3600);
        let before_upgrade = upgrade - Duration::from_secs(3600);

        let planted = [
            eval_cache_segment_key(cache, other_project, &fingerprint, uuid::Uuid::now_v7()),
            format!(
                "eval-cache-segments/{cache}/{fingerprint}/{}",
                uuid::Uuid::now_v7()
            ),
            format!("eval-cache-segments/{cache}/fp/{}", uuid::Uuid::now_v7()),
        ];
        for key in &planted {
            store.put_eval_cache(key, b"old".to_vec()).await.unwrap();
            set_last_modified(tmp.path(), key, before_upgrade);
        }
        let written_after_upgrade = eval_cache_segment_key(
            cache,
            ProjectId::now_v7(),
            &fingerprint,
            uuid::Uuid::now_v7(),
        );
        store
            .put_eval_cache(&written_after_upgrade, b"segment".to_vec())
            .await
            .unwrap();
        let cutoff = upgrade.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
            + OBJECT_STORE_CLOCK_MAX_SECONDS_AHEAD_OF_SERVER_CLOCK;

        for _ in 0..2 {
            delete_eval_cache_segments_modified_before(&store, cutoff)
                .await
                .unwrap();
            assert_eq!(
                list_eval_cache_segment_object_keys(&store).await,
                vec![written_after_upgrade.clone()]
            );
            assert!(
                store
                    .list_eval_cache_segments(cache, other_project, &fingerprint)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn an_empty_storage_migrates_without_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = NarStore::local(tmp.path()).unwrap();

        delete_eval_cache_segments_modified_before(&store, i64::MAX)
            .await
            .unwrap();

        assert!(list_eval_cache_segment_object_keys(&store).await.is_empty());
    }
}
