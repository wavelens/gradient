/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use anyhow::Result;
use gradient_entity::eval_cache_store;
use gradient_storage::NarStore;
use gradient_storage::eval_cache_segment::{
    decode, encode_bounded, eval_cache_segment_key, eval_cache_segment_prefix, merge_segments,
};
use gradient_types::ids::{CacheId, EvalCacheStoreId};
use gradient_types::{EEvalCacheStore, MEvalCacheStore};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, IntoActiveModel};
use tracing::warn;
use uuid::Uuid;

use crate::DbContext;
use crate::deliveries::pending::{PendingDeliveryKind, enqueue_after};

pub const COMPACTION_DELAY: Duration = Duration::from_secs(15 * 60);

pub struct CompactionPlan {
    listed: Vec<String>,
    merged: Vec<Vec<u8>>,
}

pub async fn record_segment(
    db: &impl ConnectionTrait,
    cache: CacheId,
    fingerprint: &str,
    copies: &[CacheId],
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    let row = MEvalCacheStore {
        id: EvalCacheStoreId::now_v7(),
        cache,
        fingerprint: fingerprint.to_owned(),
        storage_path: eval_cache_segment_prefix(cache, fingerprint),
        created_at: now,
        updated_at: now,
        ..Default::default()
    };
    EEvalCacheStore::insert(row.into_active_model())
        .on_conflict(
            OnConflict::columns([
                eval_cache_store::Column::Cache,
                eval_cache_store::Column::Fingerprint,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;

    enqueue_after(
        db,
        PendingDeliveryKind::EvalCacheCompact,
        format!("{cache}/{fingerprint}"),
        serde_json::json!({ "cache": cache, "fingerprint": fingerprint, "copies": copies }),
        COMPACTION_DELAY,
    )
    .await
}

pub async fn compact(
    ctx: &DbContext,
    cache: CacheId,
    fingerprint: &str,
    copies: &[CacheId],
) -> Result<()> {
    let nar_storage = &ctx.storage.nar_storage;
    let plan = plan_compaction(nar_storage, cache, fingerprint).await?;
    apply_compaction(
        &ctx.worker_db,
        nar_storage,
        cache,
        fingerprint,
        copies,
        plan,
    )
    .await
}

pub async fn plan_compaction(
    nar_storage: &NarStore,
    cache: CacheId,
    fingerprint: &str,
) -> Result<CompactionPlan> {
    let listed: Vec<String> = nar_storage
        .list_eval_cache_segments(cache, fingerprint)
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

pub async fn apply_compaction(
    db: &impl ConnectionTrait,
    nar_storage: &NarStore,
    cache: CacheId,
    fingerprint: &str,
    copies: &[CacheId],
    plan: CompactionPlan,
) -> Result<()> {
    let mut compacted = Vec::with_capacity(plan.merged.len());
    let mut size = 0;
    for bytes in plan.merged {
        let key = eval_cache_segment_key(cache, fingerprint, Uuid::now_v7());
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
        if let Err(e) = add_to_copy(db, nar_storage, &compacted, copy, fingerprint).await {
            warn!(%copy, %fingerprint, error = %e, "copying the compacted eval cache failed");
        }
    }

    record_compacted(db, cache, fingerprint, size).await?;
    Ok(())
}

async fn add_to_copy(
    db: &impl ConnectionTrait,
    nar_storage: &NarStore,
    compacted: &[String],
    copy: CacheId,
    fingerprint: &str,
) -> Result<()> {
    for key in compacted {
        let target = eval_cache_segment_key(copy, fingerprint, Uuid::now_v7());
        nar_storage.copy_eval_cache(key, &target).await?;
    }

    record_segment(db, copy, fingerprint, &[]).await?;
    Ok(())
}

async fn record_compacted(
    db: &impl ConnectionTrait,
    cache: CacheId,
    fingerprint: &str,
    size_bytes: i64,
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    let row = MEvalCacheStore {
        id: EvalCacheStoreId::now_v7(),
        cache,
        fingerprint: fingerprint.to_owned(),
        storage_path: eval_cache_segment_prefix(cache, fingerprint),
        size_bytes,
        created_at: now,
        updated_at: now,
        ..Default::default()
    };
    EEvalCacheStore::insert(row.into_active_model())
        .on_conflict(
            OnConflict::columns([
                eval_cache_store::Column::Cache,
                eval_cache_store::Column::Fingerprint,
            ])
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
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
    use gradient_storage::eval_cache_segment::{
        ATTR_PLACEHOLDER, EvalCacheRow, SEGMENT_MAX_BYTES, encode,
    };
    use sea_orm::{DatabaseConnection, Value};

    fn row(path: &str, kind: u8, value: &str) -> EvalCacheRow {
        EvalCacheRow {
            path: vec![path.to_owned()],
            kind,
            value: Some(value.to_owned()),
            context: None,
        }
    }

    async fn put_segment(store: &NarStore, cache: CacheId, rows: &[EvalCacheRow]) -> String {
        let key = eval_cache_segment_key(cache, "fp", Uuid::now_v7());
        store
            .put_eval_cache(&key, encode(rows).unwrap())
            .await
            .unwrap();
        key
    }

    async fn listed(store: &NarStore, cache: CacheId) -> Vec<String> {
        store
            .list_eval_cache_segments(cache, "fp")
            .await
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    fn one_exec() -> DatabaseConnection {
        mock_db().append_exec_results([exec_ok()]).into_connection()
    }

    #[tokio::test]
    async fn compaction_merges_segments_into_one() {
        let cache = CacheId::now_v7();
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        put_segment(&store, cache, &[row("a", ATTR_PLACEHOLDER, "")]).await;
        put_segment(&store, cache, &[row("a", 2, "x")]).await;
        put_segment(
            &store,
            cache,
            &[row("a", ATTR_PLACEHOLDER, ""), row("b", 2, "y")],
        )
        .await;

        let plan = plan_compaction(&store, cache, "fp").await.unwrap();
        apply_compaction(&one_exec(), &store, cache, "fp", &[], plan)
            .await
            .unwrap();

        let keys = listed(&store, cache).await;
        assert_eq!(keys.len(), 1, "{keys:?}");
        let bytes = store.get_eval_cache(&keys[0]).await.unwrap().unwrap();
        assert_eq!(
            decode(&bytes).unwrap(),
            vec![row("a", 2, "x"), row("b", 2, "y")]
        );
    }

    #[tokio::test]
    async fn compaction_splits_output_above_the_segment_size() {
        let cache = CacheId::now_v7();
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        let value = "x".repeat(1024 * 1024);
        let rows: Vec<EvalCacheRow> = (0..10).map(|n| row(&format!("c{n}"), 2, &value)).collect();
        put_segment(&store, cache, &rows[..5]).await;
        put_segment(&store, cache, &rows[5..]).await;

        let plan = plan_compaction(&store, cache, "fp").await.unwrap();
        apply_compaction(&one_exec(), &store, cache, "fp", &[], plan)
            .await
            .unwrap();

        let keys = listed(&store, cache).await;
        assert_eq!(keys.len(), 2, "{keys:?}");
        let mut compacted = Vec::new();
        for key in &keys {
            let bytes = store.get_eval_cache(key).await.unwrap().unwrap();
            assert!(bytes.len() <= SEGMENT_MAX_BYTES);
            compacted.extend(decode(&bytes).unwrap());
        }
        compacted.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(compacted, rows);
    }

    #[tokio::test]
    async fn compaction_keeps_a_segment_written_after_listing() {
        let cache = CacheId::now_v7();
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        let early = put_segment(&store, cache, &[row("a", 2, "x")]).await;

        let plan = plan_compaction(&store, cache, "fp").await.unwrap();
        let late = put_segment(&store, cache, &[row("b", 2, "y")]).await;
        apply_compaction(&one_exec(), &store, cache, "fp", &[], plan)
            .await
            .unwrap();

        let keys = listed(&store, cache).await;
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.contains(&late), "{keys:?}");
        assert!(!keys.contains(&early), "{keys:?}");
    }

    #[tokio::test]
    async fn compaction_never_deletes_segments_in_a_copy_cache() {
        let (cache, copy) = (CacheId::now_v7(), CacheId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        put_segment(&store, cache, &[row("a", 2, "x")]).await;
        let foreign = put_segment(&store, copy, &[row("b", 2, "y")]).await;
        let db = mock_db()
            .append_exec_results([exec_ok(), exec_ok(), exec_ok()])
            .into_connection();

        let plan = plan_compaction(&store, cache, "fp").await.unwrap();
        apply_compaction(&db, &store, cache, "fp", &[copy], plan)
            .await
            .unwrap();

        let keys = listed(&store, copy).await;
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.contains(&foreign), "{keys:?}");
        let added = keys.iter().find(|key| **key != foreign).unwrap();
        let bytes = store.get_eval_cache(added).await.unwrap().unwrap();
        assert_eq!(decode(&bytes).unwrap(), vec![row("a", 2, "x")]);
        let statements = raw_statements(db.into_transaction_log());
        let enqueue = statements
            .iter()
            .find(|s| s.sql.contains("ON CONFLICT (kind, key)"))
            .expect("a compaction of the copy cache is enqueued");
        let values = enqueue.values.clone().unwrap().0;
        assert_eq!(values[2], Value::String(Some(format!("{copy}/fp"))));
        assert!(
            format!("{:?}", values[3]).contains(r#"\"copies\":[]"#),
            "{values:?}"
        );
    }

    #[tokio::test]
    async fn a_push_enqueues_compaction_deduplicated_and_delayed() {
        let (cache, copy) = (CacheId::now_v7(), CacheId::now_v7());
        let db = mock_db()
            .append_exec_results([exec_ok(), exec_ok()])
            .into_connection();

        record_segment(&db, cache, "fp", &[copy]).await.unwrap();

        let statements = raw_statements(db.into_transaction_log());
        assert_eq!(statements.len(), 2, "{statements:?}");
        let enqueue = &statements[1];
        assert!(
            enqueue.sql.contains("ON CONFLICT (kind, key)"),
            "{}",
            enqueue.sql
        );
        let values = enqueue.values.clone().unwrap().0;
        assert_eq!(values[2], Value::String(Some(format!("{cache}/fp"))));
        assert_eq!(
            values[4],
            Value::BigInt(Some(COMPACTION_DELAY.as_secs() as i64))
        );
    }
}
