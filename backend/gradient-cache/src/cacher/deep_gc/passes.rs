/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The units a deep GC round walks, each idempotent and small enough to finish
//! between two checkpoints: one key shard of the NAR or log store, or a whole
//! pass where the store has no shards worth splitting on.

use super::DeepGcReport;
use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_storage::{NarStore, log_shards};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect,
};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pass {
    Blobs,
    Logs,
    Nars,
    Partials,
}

impl Pass {
    const ALL: [Pass; 4] = [Pass::Blobs, Pass::Logs, Pass::Nars, Pass::Partials];

    fn name(self) -> &'static str {
        match self {
            Pass::Blobs => "blobs",
            Pass::Logs => "logs",
            Pass::Nars => "nars",
            Pass::Partials => "partials",
        }
    }

    fn shards(self) -> Vec<String> {
        match self {
            Pass::Logs => log_shards(),
            Pass::Nars => NarStore::shards(),
            Pass::Blobs | Pass::Partials => Vec::new(),
        }
    }
}

/// Every unit of a round as its checkpoint key, `<pass>` or `<pass>/<shard>`,
/// in ascending order.
pub(super) fn units() -> Vec<String> {
    Pass::ALL
        .into_iter()
        .flat_map(|pass| {
            let shards = pass.shards();
            if shards.is_empty() {
                vec![pass.name().to_owned()]
            } else {
                shards
                    .into_iter()
                    .map(|shard| format!("{}/{shard}", pass.name()))
                    .collect()
            }
        })
        .collect()
}

pub(super) async fn run(
    state: &Arc<ServerState>,
    unit: &str,
    report: &mut DeepGcReport,
) -> Result<()> {
    let (name, shard) = unit.split_once('/').unwrap_or((unit, ""));
    let Some(pass) = Pass::ALL.into_iter().find(|p| p.name() == name) else {
        anyhow::bail!("unknown deep GC unit {unit}");
    };
    match pass {
        Pass::Blobs => pass_blobs(Arc::clone(state), report).await,
        Pass::Logs => pass_logs(Arc::clone(state), shard, report).await,
        Pass::Nars => pass_nars(Arc::clone(state), shard, report).await,
        Pass::Partials => pass_partials(state, report).await,
    }
    .with_context(|| format!("deep GC unit {unit}"))
}

async fn pass_nars(state: Arc<ServerState>, shard: &str, report: &mut DeepGcReport) -> Result<()> {
    let r = crate::cacher::reconcile_nar_shard(state, shard).await?;
    report.nars_scanned += r.orphan_nars_scanned;
    report.orphan_nars_removed += r.orphan_nars_removed;
    report.zombie_cached_paths_purged += r.zombie_cached_paths_purged;
    Ok(())
}

async fn pass_blobs(state: Arc<ServerState>, report: &mut DeepGcReport) -> Result<()> {
    let on_disk = state.nar_storage.list_blobs().await.context("list_blobs")?;
    report.blobs_scanned += on_disk.len() as u64;
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

async fn pass_logs(state: Arc<ServerState>, shard: &str, report: &mut DeepGcReport) -> Result<()> {
    let on_disk = state
        .log_storage
        .list_shard(shard)
        .await
        .context("list log shard")?;
    report.logs_scanned += on_disk.len() as u64;
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

/// The only sweep of upload partials: a walk per session, request or
/// maintenance tick stalls whoever waits on it behind the filesystem.
async fn pass_partials(state: &ServerState, report: &mut DeepGcReport) -> Result<()> {
    let ttl = std::time::Duration::from_secs(state.config.nar.partial_ttl_secs);
    let server = &state.config.server;
    for root in [
        server.nar_partial_dir(),
        server.nar_upload_partial_dir(),
        server.source_upload_partial_dir(),
    ] {
        let removed = gradient_storage::PartialStore::new(&root)?
            .gc(ttl)
            .await
            .with_context(|| format!("sweep partials under {}", root.display()))?;
        report.stale_partials_removed += removed as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cacher::test_support::test_server_state_with_log;
    use gradient_entity::ids::{BuildRequestBlobId, ProjectId};
    use gradient_storage::{FileLogStorage, LogStorage, NarStore, log_shard};
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
        pass_logs(Arc::clone(&state), &log_shard(attempt_id), &mut report)
            .await
            .unwrap();
        assert_eq!(report.orphan_logs_removed, 1);
    }

    #[test]
    fn units_ascend_so_a_checkpoint_resumes_in_order() {
        let units = units();
        assert!(units.windows(2).all(|w| w[0] < w[1]), "{units:?}");
    }

    #[tokio::test]
    async fn pass_logs_removes_a_chunk_only_orphan_and_keeps_a_referenced_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log: Arc<dyn LogStorage> = Arc::new(FileLogStorage::new(tmp.path()).await.unwrap());
        let orphan = BuildAttemptId::new(uuid::Uuid::from_u128(0x0100));
        let kept = BuildAttemptId::new(uuid::Uuid::from_u128(0x0200));
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
        pass_logs(Arc::clone(&state), "00", &mut report)
            .await
            .unwrap();
        assert_eq!(report.logs_scanned, 2);
        assert_eq!(report.orphan_logs_removed, 1);
        assert_eq!(log.list_shard("00").await.unwrap(), vec![kept]);
    }
}
