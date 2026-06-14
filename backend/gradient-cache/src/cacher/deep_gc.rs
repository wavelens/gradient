/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deep GC: bidirectional verification of every storage surface against the
//! database. Triggered by `POST /admin/maintenance/deep-gc`; runs as a
//! background task and writes progress + final state to an `admin_task` row.

use anyhow::{Context, Result};
use gradient_entity::ids::AdminTaskId;
use gradient_db::admin_tasks;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
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
}

impl DeepGcReport {
    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
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
        let key = (row.organization.into_inner(), hash);
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
    let on_disk = state.log_storage.list_logs().await.context("list_logs")?;
    report.logs_scanned = on_disk.len() as u64;
    if on_disk.is_empty() {
        return Ok(());
    }

    let referenced: HashSet<BuildId> = {
        let by_log = gradient_entity::build_attempt::Entity::find()
            .filter(gradient_entity::build_attempt::Column::LogId.is_in(on_disk.clone()))
            .all(&state.worker_db)
            .await
            .context("query build_attempts by log_id")?
            .into_iter()
            .filter_map(|a| a.log_id)
            .collect::<HashSet<_>>();
        let by_id = EBuild::find()
            .filter(CBuild::Id.is_in(on_disk.clone()))
            .all(&state.worker_db)
            .await
            .context("query builds by id")?
            .into_iter()
            .map(|b| b.id)
            .collect::<HashSet<_>>();
        by_log.into_iter().chain(by_id).collect()
    };

    for build_id in on_disk {
        if !referenced.contains(&build_id) {
            if let Err(e) = state.log_storage.delete(build_id).await {
                warn!(error = %e, %build_id, "deep_gc: failed to delete orphan log");
            } else {
                report.orphan_logs_removed += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_db::{WebDb, WorkerDb};
    use gradient_entity::ids::{BuildRequestBlobId, OrganizationId};
    use gradient_storage::{EmailSender, FileLogStorage, LogStorage, NarStore};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use gradient_test_support::fakes::email::InMemoryEmailSender;
    use gradient_test_support::log_storage::NoopLogStorage;
    use gradient_test_support::prelude::test_cli;

    fn make_state(
        nar: NarStore,
        log: Arc<dyn LogStorage>,
        db: sea_orm::DatabaseConnection,
    ) -> Arc<ServerState> {
        Arc::new(ServerState {
            web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            worker_db: WorkerDb::new(db),
            config: Arc::new(RuntimeConfig::from_cli(&test_cli()).expect("valid test config")),
            log_storage: log,
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage: nar,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_util::http::build_client().expect("http client"),
            shutdown: gradient_util::shutdown::Shutdown::new(),
            jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
            scim_group_roles: std::sync::Arc::new(Default::default()),
            board_events: tokio::sync::broadcast::channel(256).0,
            forge: gradient_forge::ForgeRegistry::with_builtin(),
            reactor: std::sync::Arc::new(gradient_db::NoReactor),
        })
    }

    #[tokio::test]
    async fn pass_blobs_removes_orphan_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let org = OrganizationId::now_v7();
        let hash = [0x11u8; 32];
        nar.put_blob(org.into_inner(), &hash, b"x".to_vec())
            .await
            .unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<gradient_entity::build_request_blob::Model, _, _>([Vec::<
                gradient_entity::build_request_blob::Model,
            >::new()])
            .into_connection();
        let state = make_state(nar, Arc::new(NoopLogStorage), db);

        let mut report = DeepGcReport::default();
        pass_blobs(Arc::clone(&state), &mut report).await.unwrap();
        assert_eq!(report.orphan_blobs_removed, 1);
        assert!(
            state
                .nar_storage
                .get_blob(org.into_inner(), &hash)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn pass_blobs_purges_zombie_row() {
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let org = OrganizationId::now_v7();
        let hash = [0x22u8; 32];

        let zombie = gradient_entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            organization: org,
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
        let bid = BuildId::now_v7();
        log.append(bid, "orphan").await.unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<gradient_entity::build::Model, _, _>(
                [Vec::<gradient_entity::build::Model>::new()],
            )
            .append_query_results::<gradient_entity::build::Model, _, _>(
                [Vec::<gradient_entity::build::Model>::new()],
            )
            .into_connection();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let state = make_state(nar, log, db);

        let mut report = DeepGcReport::default();
        pass_logs(Arc::clone(&state), &mut report).await.unwrap();
        assert_eq!(report.orphan_logs_removed, 1);
    }

    #[test]
    fn report_serialises_with_snake_case_keys() {
        let r = DeepGcReport {
            orphan_nars_removed: 1,
            zombie_blob_rows_purged: 2,
            ..Default::default()
        };
        let json = r.to_json();
        assert_eq!(json["orphan_nars_removed"], 1);
        assert_eq!(json["zombie_blob_rows_purged"], 2);
    }
}
