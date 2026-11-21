/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{Duration, NaiveDateTime};
use gradient_db::WorkerDb;
use gradient_storage::NarStore;
use gradient_storage::eval_cache_segment::{
    CheckedEvalCacheFingerprint, parse_new_layout_cache_segment_key,
};
use gradient_types::ids::{CacheId, ProjectId};
use gradient_types::*;
use gradient_util::supervision::{Pass, PassError};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;
use std::sync::Arc;

use crate::nar::shard_repair::orphan_delete_cutoff_timestamp;
use tracing::{error, info, warn};

fn select_evictions(
    rows: &[(EvalCacheStoreId, i64, NaiveDateTime)],
    max_total_bytes: u64,
    max_age: Duration,
    now: NaiveDateTime,
) -> Vec<EvalCacheStoreId> {
    let cutoff = now - max_age;

    let mut by_age: Vec<&(EvalCacheStoreId, i64, NaiveDateTime)> = rows.iter().collect();
    by_age.sort_by_key(|(_, _, updated_at)| *updated_at);

    let mut evicted: Vec<EvalCacheStoreId> = Vec::new();
    let mut surviving_total: u64 = 0;
    let mut survivors: Vec<&(EvalCacheStoreId, i64, NaiveDateTime)> = Vec::new();

    for row in &by_age {
        if row.2 < cutoff {
            evicted.push(row.0);
        } else {
            surviving_total = surviving_total.saturating_add(row.1.max(0) as u64);
            survivors.push(row);
        }
    }

    for row in survivors {
        if surviving_total <= max_total_bytes {
            break;
        }

        evicted.push(row.0);
        surviving_total = surviving_total.saturating_sub(row.1.max(0) as u64);
    }

    evicted
}

pub(crate) struct EvalCacheSweep {
    pub(crate) db: WorkerDb,
    pub(crate) config: Arc<RuntimeConfig>,
    pub(crate) nar_storage: NarStore,
    pub(crate) period: std::time::Duration,
}

impl Pass for EvalCacheSweep {
    const NAME: &'static str = "eval-cache-sweep";
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

    fn period(&self) -> std::time::Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        self.evict().await?;
        self.delete_unreferenced_eval_cache_segments_older_than_minimum_age()
            .await
            .map_err(Into::into)
    }
}

impl EvalCacheSweep {
    async fn evict(&self) -> anyhow::Result<()> {
        let cfg = &self.config.eval;

        let rows: Vec<(EvalCacheStoreId, i64, NaiveDateTime)> = EEvalCacheStore::find()
            .select_only()
            .column(CEvalCacheStore::Id)
            .column(CEvalCacheStore::SizeBytes)
            .column(CEvalCacheStore::UpdatedAt)
            .into_tuple()
            .all(&self.db)
            .await?;

        if rows.is_empty() {
            return Ok(());
        }

        let victims = select_evictions(
            &rows,
            cfg.cache_max_total_bytes,
            Duration::days(cfg.cache_max_age_days as i64),
            gradient_types::now(),
        );

        if victims.is_empty() {
            return Ok(());
        }

        let db = &self.db;
        let models = gradient_db::fetch_in_chunks(&victims, |chunk| async move {
            EEvalCacheStore::find()
                .filter(CEvalCacheStore::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?;

        let mut freed: u64 = 0;
        let mut evicted = 0usize;
        for row in models {
            if let Ok(fingerprint) = CheckedEvalCacheFingerprint::parse(&row.fingerprint)
                && let Err(e) = self
                    .nar_storage
                    .delete_eval_cache_segments(row.cache, row.project, &fingerprint)
                    .await
            {
                warn!(key = %row.storage_path, error = ?e, "eval-cache sweep: segment delete failed");
            }

            match EEvalCacheStore::delete_by_id(row.id).exec(&self.db).await {
                Ok(_) => {
                    freed = freed.saturating_add(row.size_bytes.max(0) as u64);
                    evicted += 1;
                }
                Err(e) => {
                    error!(id = %row.id, error = ?e, "eval-cache sweep: row delete failed");
                }
            }
        }

        if evicted > 0 {
            info!(
                evicted,
                freed_bytes = freed,
                "eval-cache sweep: blobs evicted"
            );
        }

        Ok(())
    }

    /// Uploads write the segment before they record the row. Segments newer than the upload grace remain.
    async fn delete_unreferenced_eval_cache_segments_older_than_minimum_age(
        &self,
    ) -> anyhow::Result<()> {
        let cutoff = orphan_delete_cutoff_timestamp(&self.config);
        let stored = self
            .nar_storage
            .list_eval_cache_segment_keys_with_last_modified_unix_seconds()
            .await?;
        let old_enough: Vec<(String, (CacheId, ProjectId, CheckedEvalCacheFingerprint))> = stored
            .into_iter()
            .filter(|(_, modified)| *modified < cutoff)
            .filter_map(|(key, _)| {
                parse_new_layout_cache_segment_key(&key).map(|store| (key, store))
            })
            .collect();
        if old_enough.is_empty() {
            return Ok(());
        }

        let rows: HashSet<(CacheId, ProjectId, String)> = EEvalCacheStore::find()
            .select_only()
            .column(CEvalCacheStore::Cache)
            .column(CEvalCacheStore::Project)
            .column(CEvalCacheStore::Fingerprint)
            .into_tuple()
            .all(&self.db)
            .await?
            .into_iter()
            .collect();

        let mut deleted = 0usize;
        for (key, (cache, project, fingerprint)) in old_enough {
            if rows.contains(&(cache, project, fingerprint.as_str().to_owned())) {
                continue;
            }
            match self.nar_storage.delete_eval_cache(&key).await {
                Ok(()) => deleted += 1,
                Err(e) => warn!(%key, error = ?e, "eval-cache sweep: segment delete failed"),
            }
        }

        if deleted > 0 {
            info!(deleted, "eval-cache sweep: deleted segments without rows");
        }

        Ok(())
    }
}

#[cfg(test)]
mod eval_cache_sweep_tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::shape::shape;
    use gradient_storage::eval_cache_segment::eval_cache_segment_key;

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn fingerprint(text: &str) -> CheckedEvalCacheFingerprint {
        CheckedEvalCacheFingerprint::parse(text).unwrap()
    }

    async fn create_test_database_with_cache_and_two_projects_sharing_fingerprint()
    -> (TestDatabase, CacheId, ProjectId, ProjectId) {
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(cache)
            .eval_cache(FINGERPRINT)
            .evaluation(EvaluationId::now_v7())
            .into_cache(cache)
            .eval_cache(FINGERPRINT)
            .into_database()
            .await;
        let rows = EEvalCacheStore::find().all(&db.connection()).await.unwrap();
        assert_eq!(rows.len(), 2);
        (db, cache, rows[0].project, rows[1].project)
    }

    async fn put_segment(
        nar_storage: &NarStore,
        cache: CacheId,
        project: ProjectId,
        fingerprint: &CheckedEvalCacheFingerprint,
    ) -> String {
        let key = eval_cache_segment_key(cache, project, fingerprint, uuid::Uuid::now_v7());
        nar_storage.put_eval_cache(&key, vec![0; 4]).await.unwrap();
        key
    }

    fn sweep(db: &TestDatabase, nar_storage: NarStore, upload_grace_hours: u32) -> EvalCacheSweep {
        let state = test_server_state(nar_storage, db.connection(), move |config| {
            config.gc.nar_upload_grace_hours = upload_grace_hours;
        });
        EvalCacheSweep {
            db: state.worker_db.clone(),
            config: Arc::clone(&state.config),
            nar_storage: state.nar_storage.clone(),
            period: std::time::Duration::from_secs(60),
        }
    }

    async fn rows_of(db: &TestDatabase, project: ProjectId) -> usize {
        EEvalCacheStore::find()
            .filter(CEvalCacheStore::Project.eq(project))
            .all(&db.connection())
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn evicting_an_expired_row_leaves_the_row_and_segments_of_another_project_with_the_same_fingerprint()
     {
        let (db, cache, expired, fresh) =
            create_test_database_with_cache_and_two_projects_sharing_fingerprint().await;
        db.execute(&format!(
            "UPDATE eval_cache_store SET updated_at = now() WHERE project = '{fresh}'"
        ))
        .await;
        let tmp = tempfile::tempdir().unwrap();
        let nar_storage = NarStore::local(tmp.path()).unwrap();
        let fp = fingerprint(FINGERPRINT);
        let evicted = put_segment(&nar_storage, cache, expired, &fp).await;
        let kept = put_segment(&nar_storage, cache, fresh, &fp).await;

        sweep(&db, nar_storage.clone(), 1).evict().await.unwrap();

        assert_eq!(rows_of(&db, expired).await, 0);
        assert_eq!(nar_storage.get_eval_cache(&evicted).await.unwrap(), None);
        assert_eq!(rows_of(&db, fresh).await, 1);
        assert!(nar_storage.get_eval_cache(&kept).await.unwrap().is_some());
        db.finish().await;
    }

    #[tokio::test]
    async fn segments_without_a_row_are_deleted_and_segments_with_a_row_stay() {
        let (db, cache, mine, other) =
            create_test_database_with_cache_and_two_projects_sharing_fingerprint().await;
        let tmp = tempfile::tempdir().unwrap();
        let nar_storage = NarStore::local(tmp.path()).unwrap();
        let (fp, fp_without_row) = (fingerprint(FINGERPRINT), fingerprint(&"f".repeat(64)));
        let kept = [
            put_segment(&nar_storage, cache, mine, &fp).await,
            put_segment(&nar_storage, cache, other, &fp).await,
        ];
        let deleted = [
            put_segment(&nar_storage, cache, mine, &fp_without_row).await,
            put_segment(&nar_storage, CacheId::now_v7(), mine, &fp).await,
            put_segment(&nar_storage, cache, ProjectId::now_v7(), &fp).await,
        ];

        sweep(&db, nar_storage.clone(), 0)
            .delete_unreferenced_eval_cache_segments_older_than_minimum_age()
            .await
            .unwrap();

        for key in &kept {
            assert!(
                nar_storage.get_eval_cache(key).await.unwrap().is_some(),
                "{key}"
            );
        }
        for key in &deleted {
            assert_eq!(
                nar_storage.get_eval_cache(key).await.unwrap(),
                None,
                "{key}"
            );
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn a_segment_without_a_row_stays_within_the_upload_grace() {
        let (db, cache, mine, _) =
            create_test_database_with_cache_and_two_projects_sharing_fingerprint().await;
        let tmp = tempfile::tempdir().unwrap();
        let nar_storage = NarStore::local(tmp.path()).unwrap();
        let uploading = put_segment(&nar_storage, cache, mine, &fingerprint(&"f".repeat(64))).await;

        sweep(&db, nar_storage.clone(), 1)
            .delete_unreferenced_eval_cache_segments_older_than_minimum_age()
            .await
            .unwrap();

        assert!(
            nar_storage
                .get_eval_cache(&uploading)
                .await
                .unwrap()
                .is_some()
        );
        db.finish().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> EvalCacheStoreId {
        EvalCacheStoreId::new(uuid::Uuid::from_u128(n))
    }

    fn ts(secs: i64) -> NaiveDateTime {
        chrono::DateTime::from_timestamp(secs, 0)
            .unwrap()
            .naive_utc()
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn empty_input_evicts_nothing() {
        let out = select_evictions(&[], 1024, Duration::days(30), ts(NOW));
        assert!(out.is_empty());
    }

    #[test]
    fn under_budget_and_fresh_evicts_nothing() {
        let rows = vec![(id(1), 100, ts(NOW - 10)), (id(2), 200, ts(NOW - 20))];
        let out = select_evictions(&rows, 1024, Duration::days(30), ts(NOW));
        assert!(out.is_empty());
    }

    #[test]
    fn aged_row_evicted_regardless_of_size() {
        let day = 86_400;
        let rows = vec![(id(1), 1, ts(NOW - 40 * day)), (id(2), 999, ts(NOW - 10))];
        let out = select_evictions(&rows, u64::MAX, Duration::days(30), ts(NOW));
        assert_eq!(out, vec![id(1)]);
    }

    #[test]
    fn over_cap_evicts_oldest_first_until_under() {
        let rows = vec![
            (id(1), 400, ts(NOW - 30)),
            (id(2), 400, ts(NOW - 20)),
            (id(3), 400, ts(NOW - 10)),
        ];
        let out = select_evictions(&rows, 800, Duration::days(30), ts(NOW));
        assert_eq!(
            out,
            vec![id(1)],
            "evict oldest until surviving total <= cap"
        );
    }

    #[test]
    fn age_and_size_combine_without_double_counting() {
        let day = 86_400;
        let rows = vec![
            (id(1), 500, ts(NOW - 40 * day)),
            (id(2), 500, ts(NOW - 30)),
            (id(3), 500, ts(NOW - 20)),
            (id(4), 500, ts(NOW - 10)),
        ];
        // Row id1 is age-evicted. Survivors id2, id3 and id4 total 1500, and evicting the oldest
        // survivor id2 is reaching the cap of 1000.
        let out = select_evictions(&rows, 1000, Duration::days(30), ts(NOW));

        assert!(out.contains(&id(1)), "aged row evicted");
        assert!(out.contains(&id(2)), "oldest surviving evicted for size");
        assert_eq!(out.len(), 2, "no id counted twice; stop once under cap");
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(unique.len(), out.len(), "no duplicate ids");
    }
}
