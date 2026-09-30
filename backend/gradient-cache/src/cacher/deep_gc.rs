/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deep GC: bidirectional verification of every storage surface against the
//! database. Triggered by `POST /admin/maintenance/deep-gc`; runs as a
//! background task and writes progress + final state to an `admin_task` row.

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_db::admin_tasks;
use gradient_entity::ids::AdminTaskId;
use gradient_types::events::gc::DeepFinished;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect,
};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{error, info, warn};

#[derive(Debug, Default, Clone, Serialize)]
pub struct DeepGcReport {
    pub nars_scanned: u64,
    pub orphan_nars_removed: u64,
    pub zombie_cached_paths_purged: u64,
    pub blobs_scanned: u64,
    pub orphan_blobs_removed: u64,
    pub zombie_blob_rows_purged: u64,
    pub blob_check_errors: u64,
    pub logs_scanned: u64,
    pub orphan_logs_removed: u64,
    pub legacy_logs_relocated: u64,
    pub legacy_logs_deleted: u64,
}

impl DeepGcReport {
    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|e| {
            warn!(error = ?e, "deep_gc: report serialization failed");
            serde_json::Value::Null
        })
    }
}

/// Entry point spawned via `state.shutdown.spawn`.
pub async fn run_deep_gc(state: Arc<ServerState>, task_id: AdminTaskId) {
    if let Err(e) = admin_tasks::mark_running(&state.worker_db, task_id).await {
        error!(error = ?e, %task_id, "deep_gc: mark_running failed");
        return;
    }

    let mut report = DeepGcReport::default();

    if let Err(e) = pass_nars(Arc::clone(&state), &mut report).await {
        return finish_failed(state, task_id, e, report).await;
    }
    flush_progress(&state, task_id, &report).await;

    if let Err(e) = pass_blobs(Arc::clone(&state), &mut report).await {
        return finish_failed(state, task_id, e, report).await;
    }
    flush_progress(&state, task_id, &report).await;

    if let Err(e) = pass_logs(Arc::clone(&state), &mut report).await {
        return finish_failed(state, task_id, e, report).await;
    }

    if let Err(e) = admin_tasks::mark_completed(&state.worker_db, task_id, report.to_json()).await {
        error!(error = ?e, %task_id, "deep_gc: mark_completed failed");
    } else {
        info!(?report, %task_id, "deep_gc completed");
    }
    state
        .record(DeepFinished {
            succeeded: true,
            report: report.to_json(),
        })
        .await;
}

async fn flush_progress(state: &Arc<ServerState>, task_id: AdminTaskId, report: &DeepGcReport) {
    if let Err(e) = admin_tasks::update_progress(&state.worker_db, task_id, report.to_json()).await
    {
        warn!(error = ?e, %task_id, "deep_gc: progress flush failed");
    }
}

async fn finish_failed(
    state: Arc<ServerState>,
    task_id: AdminTaskId,
    err: anyhow::Error,
    report: DeepGcReport,
) {
    let msg = format!("{err:#}");
    error!(%task_id, error = %msg, "deep_gc pass failed");
    if let Err(e) =
        admin_tasks::mark_failed(&state.worker_db, task_id, msg, Some(report.to_json())).await
    {
        error!(error = ?e, %task_id, "deep_gc: mark_failed failed");
    }
    state
        .record(DeepFinished {
            succeeded: false,
            report: report.to_json(),
        })
        .await;
}

async fn pass_nars(state: Arc<ServerState>, report: &mut DeepGcReport) -> Result<()> {
    let r = super::cleanup_orphaned_cache_files(state)
        .await
        .context("deep_gc: NAR pass")?;
    report.nars_scanned = r.orphan_nars_scanned;
    report.orphan_nars_removed = r.orphan_nars_removed;
    report.zombie_cached_paths_purged = r.zombie_cached_paths_purged;
    Ok(())
}

async fn pass_blobs(state: Arc<ServerState>, report: &mut DeepGcReport) -> Result<()> {
    let on_disk = state.nar_storage.list_blobs().await.context("list_blobs")?;
    report.blobs_scanned = on_disk.len() as u64;
    let on_disk_set: HashSet<(uuid::Uuid, [u8; 32])> = on_disk.iter().copied().collect();

    let rows = EBuildRequestBlob::find()
        .all(&state.worker_db)
        .await
        .context("list build_request_blob rows")?;
    let mut row_keys: HashSet<(uuid::Uuid, [u8; 32])> = HashSet::with_capacity(rows.len());
    for row in &rows {
        if row.hash.len() != 32 {
            warn!(blob_id = %row.id, "deep_gc: skipping malformed blob row hash");
            continue;
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&row.hash);
        let key = (row.project.into_inner(), hash);
        row_keys.insert(key);
        if !on_disk_set.contains(&key) {
            match state.nar_storage.get_blob(key.0, &key.1).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let blob_id = row.id;
                    if let Err(e) = row
                        .clone()
                        .into_active_model()
                        .delete(&state.worker_db)
                        .await
                    {
                        warn!(error = %e, %blob_id, "deep_gc: failed to delete zombie blob row");
                    } else {
                        report.zombie_blob_rows_purged += 1;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "deep_gc: blob probe failed");
                    report.blob_check_errors += 1;
                }
            }
        }
    }

    for key in &on_disk_set {
        if !row_keys.contains(key) {
            if let Err(e) = state.nar_storage.delete_blob(key.0, &key.1).await {
                warn!(error = %e, "deep_gc: failed to delete orphan blob");
            } else {
                report.orphan_blobs_removed += 1;
            }
        }
    }
    Ok(())
}

async fn pass_logs(state: Arc<ServerState>, report: &mut DeepGcReport) -> Result<()> {
    clean_legacy_logs(&state, report).await?;

    let on_disk = state.log_storage.list_logs().await.context("list_logs")?;
    report.logs_scanned = on_disk.len() as u64;
    if on_disk.is_empty() {
        return Ok(());
    }

    // A log key is the owning attempt's own id; it is orphan when no
    // `build_attempt` row carries that id.
    let referenced: HashSet<BuildAttemptId> = gradient_db::fetch_in_chunks(&on_disk, |chunk| {
        EBuildAttempt::find()
            .select_only()
            .column(CBuildAttempt::Id)
            .filter(CBuildAttempt::Id.is_in(chunk))
            .into_tuple::<BuildAttemptId>()
            .all(&state.worker_db)
    })
    .await
    .context("query build_attempts by id")?
    .into_iter()
    .collect();

    for attempt_id in on_disk {
        if !referenced.contains(&attempt_id) {
            if let Err(e) = state.log_storage.delete(attempt_id).await {
                warn!(error = %e, %attempt_id, "deep_gc: failed to delete orphan log");
            } else {
                report.orphan_logs_removed += 1;
            }
        }
    }
    Ok(())
}

/// Retire the pre-shard flat layout before the sweep lists the shards. A flat
/// S3 log is deleted, so its `build_log_chunk` rows would index nothing.
async fn clean_legacy_logs(state: &ServerState, report: &mut DeepGcReport) -> Result<()> {
    let cleanup = state
        .log_storage
        .clean_legacy_layout()
        .await
        .context("clean_legacy_layout")?;
    report.legacy_logs_relocated = cleanup.relocated;
    report.legacy_logs_deleted = cleanup.deleted.len() as u64;

    gradient_db::for_each_chunk(&cleanup.deleted, |chunk| {
        gradient_entity::build_log_chunk::Entity::delete_many()
            .filter(gradient_entity::build_log_chunk::Column::BuildAttempt.is_in(chunk))
            .exec(&state.worker_db)
    })
    .await
    .context("drop the chunk index of deleted legacy logs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cacher::test_support::test_server_state_with_log;
    use gradient_entity::ids::{BuildRequestBlobId, ProjectId};
    use gradient_storage::{FileLogStorage, LogStorage, NarStore};
    use gradient_test_support::log_storage::NoopLogStorage;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;

    fn make_state(
        nar: NarStore,
        log: Arc<dyn LogStorage>,
        db: sea_orm::DatabaseConnection,
    ) -> Arc<ServerState> {
        test_server_state_with_log(nar, log, db, |_| {})
    }

    #[tokio::test]
    async fn pass_blobs_removes_orphan_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let project = ProjectId::now_v7();
        let hash = [0x11u8; 32];
        nar.put_blob(project.into_inner(), &hash, b"x".to_vec())
            .await
            .unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<gradient_entity::build_request_blob::Model, _, _>([Vec::<
                gradient_entity::build_request_blob::Model,
            >::new(
            )])
            .into_connection();
        let state = make_state(nar, Arc::new(NoopLogStorage), db);

        let mut report = DeepGcReport::default();
        pass_blobs(Arc::clone(&state), &mut report).await.unwrap();
        assert_eq!(report.orphan_blobs_removed, 1);
        assert!(
            state
                .nar_storage
                .get_blob(project.into_inner(), &hash)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn pass_blobs_purges_zombie_row() {
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let project = ProjectId::now_v7();
        let hash = [0x22u8; 32];

        let zombie = gradient_entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            project,
            hash: hash.to_vec(),
            size: 1,
            created_at: now(),
            last_used_at: now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![zombie.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let state = make_state(nar, Arc::new(NoopLogStorage), db);

        let mut report = DeepGcReport::default();
        pass_blobs(Arc::clone(&state), &mut report).await.unwrap();
        assert_eq!(report.zombie_blob_rows_purged, 1);
    }

    #[tokio::test]
    async fn pass_logs_removes_orphan_log() {
        let tmp = tempfile::tempdir().unwrap();
        let nar_dir = tmp.path().join("nars");
        std::fs::create_dir_all(&nar_dir).unwrap();
        let log: Arc<dyn LogStorage> = Arc::new(FileLogStorage::new(tmp.path()).await.unwrap());
        let attempt_id = BuildAttemptId::now_v7();
        log.append(attempt_id, "orphan").await.unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<gradient_entity::build_attempt::Model, _, _>([Vec::<
                gradient_entity::build_attempt::Model,
            >::new(
            )])
            .into_connection();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let state = make_state(nar, log, db);

        let mut report = DeepGcReport::default();
        pass_logs(Arc::clone(&state), &mut report).await.unwrap();
        assert_eq!(report.orphan_logs_removed, 1);
    }

    #[tokio::test]
    async fn pass_logs_deletes_flat_s3_logs_and_their_chunk_index() {
        use gradient_storage::S3LogStorage;
        use object_store::{
            ObjectStore as _, ObjectStoreExt as _, PutPayload, memory::InMemory,
            path::Path as ObjectPath,
        };

        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemory::new());
        let legacy = BuildAttemptId::now_v7();
        store
            .put(
                &ObjectPath::from(format!("logs/{legacy}/chunk_00000000.zst")),
                PutPayload::from_static(b"old"),
            )
            .await
            .unwrap();
        let log: Arc<dyn LogStorage> = Arc::new(S3LogStorage::new(
            FileLogStorage::new(tmp.path()).await.unwrap(),
            store.clone(),
            "",
        ));

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let state = make_state(nar, log, db);

        let mut report = DeepGcReport::default();
        pass_logs(Arc::clone(&state), &mut report).await.unwrap();

        assert_eq!(report.legacy_logs_deleted, 1);
        assert_eq!(report.logs_scanned, 0);
        assert!(
            store
                .list_with_delimiter(Some(&ObjectPath::from("logs")))
                .await
                .unwrap()
                .common_prefixes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pass_logs_removes_a_chunk_only_orphan_and_keeps_a_referenced_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log: Arc<dyn LogStorage> = Arc::new(FileLogStorage::new(tmp.path()).await.unwrap());
        let orphan = BuildAttemptId::now_v7();
        let kept = BuildAttemptId::now_v7();
        log.write_chunk(orphan, 0, b"z").await.unwrap();
        log.write_chunk(kept, 0, b"z").await.unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![std::collections::BTreeMap::from([(
                "id",
                sea_orm::Value::from(kept.into_inner()),
            )])]])
            .into_connection();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let state = make_state(nar, Arc::clone(&log), db);

        let mut report = DeepGcReport::default();
        pass_logs(Arc::clone(&state), &mut report).await.unwrap();
        assert_eq!(report.logs_scanned, 2);
        assert_eq!(report.orphan_logs_removed, 1);
        assert_eq!(log.list_logs().await.unwrap(), vec![kept]);
    }
}
