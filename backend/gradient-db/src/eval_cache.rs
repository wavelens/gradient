/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use gradient_entity::eval_cache_store;
use gradient_storage::NarStore;
use gradient_storage::eval_cache_segment::{
    CheckedEvalCacheFingerprint, decode, encode_bounded, eval_cache_segment_key,
    eval_cache_segment_prefix, merge_segments,
};
use gradient_types::ids::{CacheId, EvalCacheStoreId, ProjectId};
use gradient_types::{EEvalCacheStore, MEvalCacheStore};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, IntoActiveModel};
use serde::Deserialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::DbContext;
use crate::deliveries::pending::{PendingDeliveryKind, enqueue_after};
use crate::maintenance::storage_migrations::{
    DELETE_EVALUATION_CACHE_SEGMENTS_WRITTEN_BEFORE_UPGRADE_MIGRATION_NAME,
    is_storage_migration_applied,
};

pub const COMPACTION_DELAY: Duration = Duration::from_secs(15 * 60);

const EVAL_CACHE_STORE_UNIQUE_INDEX_COLUMNS: [eval_cache_store::Column; 3] = [
    eval_cache_store::Column::Cache,
    eval_cache_store::Column::Project,
    eval_cache_store::Column::Fingerprint,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalCacheStoreRowKey {
    pub cache: CacheId,
    pub project: ProjectId,
    pub fingerprint: CheckedEvalCacheFingerprint,
}

impl EvalCacheStoreRowKey {
    fn in_cache(&self, cache: CacheId) -> Self {
        Self {
            cache,
            ..self.clone()
        }
    }

    fn new_segment_key(&self) -> String {
        eval_cache_segment_key(self.cache, self.project, &self.fingerprint, Uuid::now_v7())
    }

    fn row(&self, size_bytes: i64) -> MEvalCacheStore {
        let now = gradient_types::now();
        MEvalCacheStore {
            id: EvalCacheStoreId::now_v7(),
            cache: self.cache,
            project: self.project,
            fingerprint: self.fingerprint.to_string(),
            storage_path: eval_cache_segment_prefix(self.cache, self.project, &self.fingerprint),
            size_bytes,
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }
}

impl fmt::Display for EvalCacheStoreRowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.cache, self.project, self.fingerprint)
    }
}

#[derive(Deserialize)]
struct QueuedCompactionPayload {
    cache: CacheId,
    project: Option<ProjectId>,
    fingerprint: String,
    copies: Vec<CacheId>,
}

pub struct DecodedCompactionJob {
    pub store: EvalCacheStoreRowKey,
    pub copies: Vec<CacheId>,
}

/// A compaction queued before compactions named their project has no project. It returns `None`,
/// so the caller drops it instead of compacting segments of every project.
pub fn decode_queued_compaction_payload(
    payload: &serde_json::Value,
) -> Result<Option<DecodedCompactionJob>> {
    let payload: QueuedCompactionPayload =
        serde_json::from_value(payload.clone()).context("decode queued eval cache compaction")?;
    let Some(project) = payload.project else {
        return Ok(None);
    };
    Ok(Some(DecodedCompactionJob {
        store: EvalCacheStoreRowKey {
            cache: payload.cache,
            project,
            fingerprint: CheckedEvalCacheFingerprint::parse(&payload.fingerprint)?,
        },
        copies: payload.copies,
    }))
}

struct CompactionPlan {
    listed: Vec<String>,
    merged: Vec<Vec<u8>>,
}

pub async fn record_segment(
    db: &impl ConnectionTrait,
    store: &EvalCacheStoreRowKey,
    copies: &[CacheId],
) -> Result<(), DbErr> {
    EEvalCacheStore::insert(store.row(0).into_active_model())
        .on_conflict(
            OnConflict::columns(EVAL_CACHE_STORE_UNIQUE_INDEX_COLUMNS)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;

    enqueue_after(
        db,
        PendingDeliveryKind::EvalCacheCompact,
        store.to_string(),
        serde_json::json!({
            "cache": store.cache,
            "project": store.project,
            "fingerprint": store.fingerprint.as_str(),
            "copies": copies,
        }),
        COMPACTION_DELAY,
    )
    .await
}

// Segments written before the upgrade can come from a worker of another project. Pull and compaction don't read those segments until the storage migration has deleted them. Store only a true answer. The storage migration can't reverse after the first application.
pub async fn is_pre_upgrade_eval_cache_segment_cleanup_applied(
    db: &impl ConnectionTrait,
) -> Result<bool> {
    static APPLIED: AtomicBool = AtomicBool::new(false);
    if APPLIED.load(Ordering::Relaxed) {
        return Ok(true);
    }
    let applied = is_storage_migration_applied(
        db,
        DELETE_EVALUATION_CACHE_SEGMENTS_WRITTEN_BEFORE_UPGRADE_MIGRATION_NAME,
    )
    .await?;
    if applied {
        APPLIED.store(true, Ordering::Relaxed);
    }
    Ok(applied)
}

pub async fn compact(
    ctx: &DbContext,
    store: &EvalCacheStoreRowKey,
    copies: &[CacheId],
) -> Result<()> {
    if !is_pre_upgrade_eval_cache_segment_cleanup_applied(&ctx.worker_db).await? {
        info!(
            %store,
            "The code doesn't compact the eval cache until the one-time deletion can apply to segments written before the upgrade."
        );
        return Ok(());
    }
    let nar_storage = &ctx.storage.nar_storage;
    let plan = plan_compaction(nar_storage, store).await?;
    apply_compaction(&ctx.worker_db, nar_storage, store, copies, plan).await
}

async fn plan_compaction(
    nar_storage: &NarStore,
    store: &EvalCacheStoreRowKey,
) -> Result<CompactionPlan> {
    let listed: Vec<String> = nar_storage
        .list_eval_cache_segments(store.cache, store.project, &store.fingerprint)
        .await?
        .into_iter()
        .map(|(key, _)| key)
        .collect();

    let mut segments = Vec::with_capacity(listed.len());
    for key in &listed {
        let Some(bytes) = nar_storage.get_eval_cache(key).await? else {
            continue;
        };

        match decode(&bytes) {
            Ok(rows) => segments.push(rows),
            Err(e) => warn!(%key, error = %e, "dropping an undecodable eval cache segment"),
        }
    }

    let merged = encode_bounded(&merge_segments(segments))?;
    Ok(CompactionPlan { listed, merged })
}

async fn apply_compaction(
    db: &impl ConnectionTrait,
    nar_storage: &NarStore,
    store: &EvalCacheStoreRowKey,
    copies: &[CacheId],
    plan: CompactionPlan,
) -> Result<()> {
    let mut compacted = Vec::with_capacity(plan.merged.len());
    let mut size = 0;
    for bytes in plan.merged {
        let key = store.new_segment_key();
        size += bytes.len() as i64;
        nar_storage.put_eval_cache(&key, bytes).await?;
        compacted.push(key);
    }

    for key in &plan.listed {
        nar_storage.delete_eval_cache(key).await?;
    }

    if compacted.is_empty() {
        return Ok(());
    }

    for &copy in copies {
        let copy_store = store.in_cache(copy);
        if let Err(e) = add_to_copy(db, nar_storage, &compacted, &copy_store).await {
            warn!(%copy, %store, error = %e, "copying the compacted eval cache failed");
        }
    }

    record_compacted(db, store, size).await?;
    Ok(())
}

async fn add_to_copy(
    db: &impl ConnectionTrait,
    nar_storage: &NarStore,
    compacted: &[String],
    copy_store: &EvalCacheStoreRowKey,
) -> Result<()> {
    for key in compacted {
        nar_storage
            .copy_eval_cache(key, &copy_store.new_segment_key())
            .await?;
    }

    record_segment(db, copy_store, &[]).await?;
    Ok(())
}

async fn record_compacted(
    db: &impl ConnectionTrait,
    store: &EvalCacheStoreRowKey,
    size_bytes: i64,
) -> Result<(), DbErr> {
    EEvalCacheStore::insert(store.row(size_bytes).into_active_model())
        .on_conflict(
            OnConflict::columns(EVAL_CACHE_STORE_UNIQUE_INDEX_COLUMNS)
                .update_columns([
                    eval_cache_store::Column::StoragePath,
                    eval_cache_store::Column::SizeBytes,
                    eval_cache_store::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
    use gradient_fixtures::rows;
    use gradient_storage::eval_cache_segment::{
        ATTR_PLACEHOLDER, EvalCacheRow, SEGMENT_MAX_BYTES, encode,
    };
    use gradient_types::{ECache, EProject, EUser};
    use sea_orm::{DatabaseConnection, Value};
    use sea_orm_migration::{MigratorTrait, SchemaManager};

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn store_key(cache: CacheId, project: ProjectId) -> EvalCacheStoreRowKey {
        EvalCacheStoreRowKey {
            cache,
            project,
            fingerprint: CheckedEvalCacheFingerprint::parse(FINGERPRINT).unwrap(),
        }
    }

    fn row(path: &str, kind: u8, value: &str) -> EvalCacheRow {
        EvalCacheRow {
            path: vec![path.to_owned()],
            kind,
            value: Some(value.to_owned()),
            context: None,
        }
    }

    async fn put_segment(
        nar_storage: &NarStore,
        store: &EvalCacheStoreRowKey,
        rows: &[EvalCacheRow],
    ) -> String {
        let key = store.new_segment_key();
        nar_storage
            .put_eval_cache(&key, encode(rows).unwrap())
            .await
            .unwrap();
        key
    }

    async fn listed(nar_storage: &NarStore, store: &EvalCacheStoreRowKey) -> Vec<String> {
        nar_storage
            .list_eval_cache_segments(store.cache, store.project, &store.fingerprint)
            .await
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    fn one_exec() -> DatabaseConnection {
        mock_db().append_exec_results([exec_ok()]).into_connection()
    }

    async fn database_with_a_cache_and_two_projects()
    -> (TestDatabase, CacheId, ProjectId, ProjectId) {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        crate::rows::insert::<EUser, _>(&connection, rows::user())
            .await
            .expect("the user is written");
        let mine = crate::rows::insert::<EProject, _>(&connection, rows::project())
            .await
            .expect("the project is written");
        let other = crate::rows::insert::<EProject, _>(
            &connection,
            rows::project_with_id(ProjectId::now_v7(), "other-project"),
        )
        .await
        .expect("the other project is written");
        let cache = crate::rows::insert::<ECache, _>(&connection, rows::cache())
            .await
            .expect("the cache is written");
        (db, cache.id, mine.id, other.id)
    }

    #[tokio::test]
    async fn compaction_without_old_segment_cleanup_writes_nothing_and_leaves_old_segments_unchanged()
     {
        let store = store_key(CacheId::now_v7(), ProjectId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let cleanup_not_applied = mock_db()
            .append_query_results([Vec::<std::collections::BTreeMap<String, Value>>::new()])
            .into_connection();
        let (ctx, _pool) = crate::test_ctx::ctx_at(cleanup_not_applied, dir.path()).await;
        let nar_storage = &ctx.storage.nar_storage;
        let planted = put_segment(nar_storage, &store, &[row("a", 2, "planted")]).await;
        let uploaded = put_segment(nar_storage, &store, &[row("b", 2, "uploaded")]).await;

        compact(&ctx, &store, &[CacheId::now_v7()]).await.unwrap();

        let mut keys = listed(nar_storage, &store).await;
        keys.sort_unstable();
        let mut expected = vec![planted, uploaded];
        expected.sort_unstable();
        assert_eq!(keys, expected);
        crate::test_ctx::settle(ctx).await;
    }

    #[tokio::test]
    async fn compaction_merges_segments_into_one() {
        let store = store_key(CacheId::now_v7(), ProjectId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let nar_storage = NarStore::local(dir.path()).unwrap();
        put_segment(&nar_storage, &store, &[row("a", ATTR_PLACEHOLDER, "")]).await;
        put_segment(&nar_storage, &store, &[row("a", 2, "x")]).await;
        put_segment(
            &nar_storage,
            &store,
            &[row("a", ATTR_PLACEHOLDER, ""), row("b", 2, "y")],
        )
        .await;

        let plan = plan_compaction(&nar_storage, &store).await.unwrap();
        apply_compaction(&one_exec(), &nar_storage, &store, &[], plan)
            .await
            .unwrap();

        let keys = listed(&nar_storage, &store).await;
        assert_eq!(keys.len(), 1, "{keys:?}");
        let bytes = nar_storage.get_eval_cache(&keys[0]).await.unwrap().unwrap();
        assert_eq!(
            decode(&bytes).unwrap(),
            vec![row("a", 2, "x"), row("b", 2, "y")]
        );
    }

    #[tokio::test]
    async fn compaction_splits_output_above_the_segment_size() {
        let store = store_key(CacheId::now_v7(), ProjectId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let nar_storage = NarStore::local(dir.path()).unwrap();
        let value = "x".repeat(1024 * 1024);
        let rows: Vec<EvalCacheRow> = (0..10).map(|n| row(&format!("c{n}"), 2, &value)).collect();
        put_segment(&nar_storage, &store, &rows[..5]).await;
        put_segment(&nar_storage, &store, &rows[5..]).await;

        let plan = plan_compaction(&nar_storage, &store).await.unwrap();
        apply_compaction(&one_exec(), &nar_storage, &store, &[], plan)
            .await
            .unwrap();

        let keys = listed(&nar_storage, &store).await;
        assert_eq!(keys.len(), 2, "{keys:?}");
        let mut compacted = Vec::new();
        for key in &keys {
            let bytes = nar_storage.get_eval_cache(key).await.unwrap().unwrap();
            assert!(bytes.len() <= SEGMENT_MAX_BYTES);
            compacted.extend(decode(&bytes).unwrap());
        }
        compacted.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(compacted, rows);
    }

    #[tokio::test]
    async fn compaction_keeps_a_segment_written_after_listing() {
        let store = store_key(CacheId::now_v7(), ProjectId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let nar_storage = NarStore::local(dir.path()).unwrap();
        let early = put_segment(&nar_storage, &store, &[row("a", 2, "x")]).await;

        let plan = plan_compaction(&nar_storage, &store).await.unwrap();
        let late = put_segment(&nar_storage, &store, &[row("b", 2, "y")]).await;
        apply_compaction(&one_exec(), &nar_storage, &store, &[], plan)
            .await
            .unwrap();

        let keys = listed(&nar_storage, &store).await;
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.contains(&late), "{keys:?}");
        assert!(!keys.contains(&early), "{keys:?}");
    }

    #[tokio::test]
    async fn compaction_never_deletes_segments_in_a_copy_cache() {
        let store = store_key(CacheId::now_v7(), ProjectId::now_v7());
        let copy_store = store.in_cache(CacheId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let nar_storage = NarStore::local(dir.path()).unwrap();
        put_segment(&nar_storage, &store, &[row("a", 2, "x")]).await;
        let foreign = put_segment(&nar_storage, &copy_store, &[row("b", 2, "y")]).await;
        let db = mock_db()
            .append_exec_results([exec_ok(), exec_ok(), exec_ok()])
            .into_connection();

        let plan = plan_compaction(&nar_storage, &store).await.unwrap();
        apply_compaction(&db, &nar_storage, &store, &[copy_store.cache], plan)
            .await
            .unwrap();

        let keys = listed(&nar_storage, &copy_store).await;
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.contains(&foreign), "{keys:?}");
        let added = keys.iter().find(|key| **key != foreign).unwrap();
        let bytes = nar_storage.get_eval_cache(added).await.unwrap().unwrap();
        assert_eq!(decode(&bytes).unwrap(), vec![row("a", 2, "x")]);
        let statements = raw_statements(db.into_transaction_log());
        let enqueue = statements
            .iter()
            .find(|s| s.sql.contains("ON CONFLICT (kind, key)"))
            .expect("a compaction of the copy cache is enqueued");
        let values = enqueue.values.clone().unwrap().0;
        assert_eq!(values[2], Value::String(Some(copy_store.to_string())));
        assert!(
            format!("{:?}", values[3]).contains(r#"\"copies\":[]"#),
            "{values:?}"
        );
    }

    #[tokio::test]
    async fn a_push_enqueues_compaction_deduplicated_and_delayed() {
        let store = store_key(CacheId::now_v7(), ProjectId::now_v7());
        let db = mock_db()
            .append_exec_results([exec_ok(), exec_ok()])
            .into_connection();

        record_segment(&db, &store, &[CacheId::now_v7()])
            .await
            .unwrap();

        let statements = raw_statements(db.into_transaction_log());
        assert_eq!(statements.len(), 2, "{statements:?}");
        let enqueue = &statements[1];
        let values = enqueue.values.clone().unwrap().0;
        assert_eq!(values[2], Value::String(Some(store.to_string())));
        assert!(
            format!("{:?}", values[3]).contains(&format!(r#"\"project\":\"{}\""#, store.project)),
            "{values:?}"
        );
        assert_eq!(
            values[4],
            Value::BigInt(Some(COMPACTION_DELAY.as_secs() as i64))
        );
    }

    #[tokio::test]
    async fn compacting_one_project_leaves_the_segments_and_row_of_another() {
        let (db, cache, mine, other) = database_with_a_cache_and_two_projects().await;
        let connection = db.connection();
        let dir = tempfile::TempDir::new().unwrap();
        let nar_storage = NarStore::local(dir.path()).unwrap();
        let (mine, other) = (store_key(cache, mine), store_key(cache, other));
        record_segment(&connection, &mine, &[]).await.unwrap();
        record_segment(&connection, &other, &[]).await.unwrap();
        put_segment(&nar_storage, &mine, &[row("a", 2, "mine")]).await;
        let foreign = put_segment(&nar_storage, &other, &[row("a", 2, "other")]).await;

        let plan = plan_compaction(&nar_storage, &mine).await.unwrap();
        apply_compaction(&connection, &nar_storage, &mine, &[], plan)
            .await
            .unwrap();

        let compacted = listed(&nar_storage, &mine).await;
        assert_eq!(compacted.len(), 1, "{compacted:?}");
        let bytes = nar_storage
            .get_eval_cache(&compacted[0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(decode(&bytes).unwrap(), vec![row("a", 2, "mine")]);
        assert_eq!(listed(&nar_storage, &other).await, vec![foreign]);
        let stored = db.rows::<EEvalCacheStore>().await;
        let size_of = |project| {
            stored
                .iter()
                .find(|row| row.project == project)
                .map(|row| row.size_bytes)
        };
        assert_eq!(stored.len(), 2, "{stored:?}");
        assert!(
            size_of(mine.project).is_some_and(|size| size > 0),
            "{stored:?}"
        );
        assert_eq!(size_of(other.project), Some(0), "{stored:?}");
        db.finish().await;
    }

    #[tokio::test]
    async fn eval_cache_project_migrations_keep_rows_without_project_for_old_binary_then_delete_them_swap_unique_index_and_reapply()
     {
        let (db, cache, mine, other) = database_with_a_cache_and_two_projects().await;
        let connection = db.connection();
        let migration_named = |prefix: &str| {
            gradient_migration::Migrator::migrations()
                .into_iter()
                .find(|m| m.name().starts_with(prefix))
                .expect("register the eval cache project migrations")
        };
        let add_project = migration_named("m20261230_000000_");
        let swap_unique_index = migration_named("m20261230_000003_");
        let manager = SchemaManager::new(&connection);
        let old_server_insert_eval_cache_store_row_without_project_sql = |fingerprint: &str| {
            format!(
                "INSERT INTO eval_cache_store (id, cache, fingerprint, storage_path, size_bytes, \
                 created_at, updated_at) VALUES \
                 (gen_random_uuid(), '{cache}', '{fingerprint}', 'old', 1, now(), now()) \
                 ON CONFLICT (cache, fingerprint) DO UPDATE SET updated_at = now()"
            )
        };
        let build_eval_cache_store_insert_sql = |project: ProjectId, fingerprint: &str| {
            format!(
                "INSERT INTO eval_cache_store (id, cache, project, fingerprint, storage_path, \
                 size_bytes, created_at, updated_at) VALUES \
                 (gen_random_uuid(), '{cache}', '{project}', '{fingerprint}', 'new', 0, now(), now())"
            )
        };
        let count_eval_cache_store_rows_with_and_without_project_as_text = || {
            db.texts(
                "SELECT count(project)::text || ' with, ' || \
                 (count(*) - count(project))::text || ' without' FROM eval_cache_store",
            )
        };
        swap_unique_index
            .down(&manager)
            .await
            .expect("the index swap reverts");
        add_project
            .down(&manager)
            .await
            .expect("the project column migration reverts");
        db.execute(&old_server_insert_eval_cache_store_row_without_project_sql(
            FINGERPRINT,
        ))
        .await;

        add_project
            .up(&manager)
            .await
            .expect("the project column migration runs");
        let old_server_insert_during_upgrade = connection
            .execute_unprepared(&old_server_insert_eval_cache_store_row_without_project_sql(
                "written-by-old-server",
            ))
            .await;
        db.execute(&build_eval_cache_store_insert_sql(mine, "kept"))
            .await;
        let rows_before_swap = count_eval_cache_store_rows_with_and_without_project_as_text().await;
        swap_unique_index
            .up(&manager)
            .await
            .expect("the index swap runs");
        let rows_after_swap = count_eval_cache_store_rows_with_and_without_project_as_text().await;
        let old_server_insert_after_swap = connection
            .execute_unprepared(&old_server_insert_eval_cache_store_row_without_project_sql(
                FINGERPRINT,
            ))
            .await;
        let first = connection
            .execute_unprepared(&build_eval_cache_store_insert_sql(mine, FINGERPRINT))
            .await;
        let same_fingerprint_other_project = connection
            .execute_unprepared(&build_eval_cache_store_insert_sql(other, FINGERPRINT))
            .await;
        let duplicate = connection
            .execute_unprepared(&build_eval_cache_store_insert_sql(mine, FINGERPRINT))
            .await;
        swap_unique_index
            .down(&manager)
            .await
            .expect("the index swap reverts with rows of two projects");
        add_project
            .down(&manager)
            .await
            .expect("the project column migration reverts");
        let rows_after_down = db
            .texts("SELECT fingerprint::text FROM eval_cache_store ORDER BY 1")
            .await;
        let project_columns = db
            .texts(
                "SELECT column_name::text FROM information_schema.columns \
                 WHERE table_name = 'eval_cache_store' AND column_name = 'project'",
            )
            .await;
        add_project
            .up(&manager)
            .await
            .expect("the project column migration runs again");
        swap_unique_index
            .up(&manager)
            .await
            .expect("the index swap runs again");

        assert!(
            old_server_insert_during_upgrade.is_ok(),
            "{old_server_insert_during_upgrade:?}"
        );
        assert_eq!(rows_before_swap, vec!["1 with, 2 without"]);
        assert_eq!(rows_after_swap, vec!["1 with, 0 without"]);
        assert!(
            old_server_insert_after_swap.is_err(),
            "{old_server_insert_after_swap:?}"
        );
        assert!(first.is_ok(), "{first:?}");
        assert!(
            same_fingerprint_other_project.is_ok(),
            "{same_fingerprint_other_project:?}"
        );
        assert!(duplicate.is_err(), "{duplicate:?}");
        assert_eq!(
            rows_after_down,
            vec![FINGERPRINT.to_string(), "kept".to_string()]
        );
        assert!(project_columns.is_empty(), "{project_columns:?}");
        db.finish().await;
    }

    #[tokio::test]
    async fn deleting_project_deletes_eval_cache_store_rows() {
        let (db, cache, mine, other) = database_with_a_cache_and_two_projects().await;
        let connection = db.connection();
        for project in [mine, other] {
            connection
                .execute_unprepared(&format!(
                    "INSERT INTO eval_cache_store (id, cache, project, fingerprint, storage_path, \
                     size_bytes, created_at, updated_at) VALUES \
                     (gen_random_uuid(), '{cache}', '{project}', '{FINGERPRINT}', 'new', 0, now(), now())"
                ))
                .await
                .expect("the row is written");
        }

        db.execute(&format!("DELETE FROM project WHERE id = '{mine}'"))
            .await;

        let projects_left = db.texts("SELECT project::text FROM eval_cache_store").await;
        assert_eq!(projects_left, vec![other.to_string()]);
        db.finish().await;
    }

    #[tokio::test]
    async fn eval_cache_store_project_column_migration_applied_at_is_read_from_seaql_migrations_and_unapplied_is_none()
     {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        let before_test = chrono::Utc::now().timestamp() - 3600;

        let applied_at = crate::maintenance::storage_migrations::migration_applied_at_unix_seconds(
            &connection,
            crate::maintenance::storage_migrations::add_project_to_eval_cache_store_migration_version(),
        )
        .await
        .expect("seaql_migrations is readable");
        let missing = crate::maintenance::storage_migrations::migration_applied_at_unix_seconds(
            &connection,
            "m00000000_000000_never_applied",
        )
        .await
        .expect("seaql_migrations is readable");

        assert!(
            applied_at.is_some_and(|at| at > before_test),
            "{applied_at:?}"
        );
        assert_eq!(missing, None);
        db.finish().await;
    }
}
