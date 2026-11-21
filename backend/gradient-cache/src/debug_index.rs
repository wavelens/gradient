/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WorkerDb;
use gradient_storage::NarStore;
use gradient_util::supervision::{Pass, PassError};
use std::time::Duration;
use tracing::{debug, warn};

const DEBUG_INDEX_BATCH: u64 = 32;

pub(crate) struct DebugIndex {
    pub(crate) db: WorkerDb,
    pub(crate) nar_storage: NarStore,
    pub(crate) period: Duration,
}

impl Pass for DebugIndex {
    const NAME: &'static str = "debug-index";
    const BUDGET: Duration = Duration::from_secs(600);

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        self.index().await.map_err(Into::into)
    }
}

impl DebugIndex {
    async fn index(&self) -> anyhow::Result<()> {
        let pending =
            gradient_db::caches::debug_info::pending_debug_index(&self.db, DEBUG_INDEX_BATCH)
                .await?;
        if pending.is_empty() {
            return Ok(());
        }

        let mut build_ids = 0usize;
        for path in &pending {
            match gradient_db::caches::debug_info::index_cached_path(
                &self.db,
                &self.nar_storage,
                path.id,
            )
            .await
            {
                Ok(count) => build_ids += count,
                Err(e) => warn!(hash = %path.hash, error = %e, "debug index backfill failed"),
            }
        }

        debug!(
            scanned = pending.len(),
            build_ids, "debug-info backfill pass complete"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
    use gradient_entity::debug_info::Entity as EDebugInfo;
    use gradient_types::ids::EvaluationId;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    #[tokio::test]
    async fn debug_index_reads_each_content_variant_object_at_its_own_key() {
        let hash = "dbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdb";
        let with_debug_file = format!("{hash}-withdebugfi1e");
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(hash, "hello-2.12.1-debug")
            .into_database()
            .await;
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             SELECT uuidv7(), hash, '{with_debug_file}', package, 'sha256:other', file_size, \
                 nar_size, 'sha256:othernar', false, true, created_at \
             FROM cached_path WHERE hash = '{hash}';"
        ))
        .await;
        let (nar, build_id) =
            gradient_test_support::cache_fixture::compressed_nar_with_one_debug_file_and_its_build_id()
                .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let nar_storage = NarStore::local(tmp.path()).expect("store");
        nar_storage.put(&with_debug_file, nar).await.expect("put");

        DebugIndex {
            db: WorkerDb::new(db.connection()),
            nar_storage,
            period: Duration::from_secs(60),
        }
        .index()
        .await
        .expect("the debug index pass runs");

        let variants = ECachedPath::find()
            .filter(CCachedPath::Hash.eq(hash))
            .all(&db.connection())
            .await
            .expect("the stored contents are read");
        let indexed: Vec<(String, _)> = EDebugInfo::find()
            .all(&db.connection())
            .await
            .expect("the debug info rows are read")
            .into_iter()
            .map(|row| (row.build_id, row.cached_path))
            .collect();
        let owner = variants
            .iter()
            .find(|row| row.nar_object_storage_key == with_debug_file)
            .expect("the variant with the debug file is stored")
            .id;
        assert_eq!(indexed, vec![(build_id, owner)]);
        assert!(variants.iter().all(|row| row.debug_info_indexed));
        db.finish().await;
    }
}
