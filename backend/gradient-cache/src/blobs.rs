/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::sync::Arc;
use tracing::{info, warn};

pub async fn cleanup_unused_build_request_blobs(state: Arc<ServerState>) -> Result<()> {
    let ttl_hours = state.config.gc.nar_ttl_hours;
    if ttl_hours == 0 {
        return Ok(());
    }

    let cutoff = now() - chrono::Duration::hours(ttl_hours as i64);
    let stale = EBuildRequestBlob::find()
        .filter(CBuildRequestBlob::LastUsedAt.lt(cutoff))
        .all(&state.worker_db)
        .await
        .context("Failed to query stale build_request_blob rows")?;

    let mut removed = 0u64;
    for blob in stale {
        if blob.hash.len() != 32 {
            warn!(blob_id = %blob.id, "skipping build_request_blob with malformed hash");
            continue;
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&blob.hash);
        let blob_id = blob.id;
        let project_id = blob.project;
        if let Err(e) = blob.into_active_model().delete(&state.worker_db).await {
            warn!(error = %e, %blob_id, "failed to delete build_request_blob row");
            continue;
        }
        if let Err(e) = state
            .nar_storage
            .delete_blob(project_id.into_inner(), &hash)
            .await
        {
            warn!(error = %e, %blob_id, "failed to delete build-request blob payload");
        }
        removed += 1;
    }

    if removed > 0 {
        info!(count = removed, "Removed stale build-request blobs");
        state
            .record(Swept {
                pass: Pass::BuildRequestBlobs,
                removed,
            })
            .await;
    }
    Ok(())
}

pub(crate) async fn cleanup_expired_upload_sessions(state: Arc<ServerState>) -> Result<()> {
    let res = EUploadSession::delete_many()
        .filter(CUploadSession::ExpiresAt.lt(now()))
        .filter(CUploadSession::DispatchedAt.is_null())
        .exec(&state.worker_db)
        .await
        .context("Failed to delete expired upload_session rows")?;

    if res.rows_affected > 0 {
        info!(count = res.rows_affected, "Removed expired upload sessions");
        state
            .record(Swept {
                pass: Pass::UploadSessions,
                removed: res.rows_affected,
            })
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_storage::NarStore;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::path::Path;

    fn state_with_worker_db(base: &Path, db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let nar_storage = NarStore::local(base.to_str().unwrap()).unwrap();
        test_server_state(nar_storage, db, |config| {
            config.gc.nar_ttl_hours = 24;
        })
    }

    #[tokio::test]
    async fn build_request_blob_sweep_evicts_stale() {
        use gradient_entity::ids::{BuildRequestBlobId, ProjectId};

        let tmp = tempfile::tempdir().unwrap();
        let project = ProjectId::now_v7();
        let hash = [0xABu8; 32];
        let stale = gradient_entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            project,
            hash: hash.to_vec(),
            size: 1,
            created_at: now() - chrono::Duration::days(30),
            last_used_at: now() - chrono::Duration::days(30),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stale.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let state = state_with_worker_db(tmp.path(), db);

        state
            .nar_storage
            .put_blob(project.into_inner(), &hash, b"payload".to_vec())
            .await
            .unwrap();

        cleanup_unused_build_request_blobs(Arc::clone(&state))
            .await
            .unwrap();

        assert!(
            state
                .nar_storage
                .get_blob(project.into_inner(), &hash)
                .await
                .unwrap()
                .is_none(),
            "stale blob payload must be removed from storage"
        );
    }

    #[tokio::test]
    async fn build_request_blob_sweep_disabled_when_ttl_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_server_state(nar_storage, db, |config| {
            config.gc.nar_ttl_hours = 0;
        });

        cleanup_unused_build_request_blobs(state).await.unwrap();
    }

    #[tokio::test]
    async fn build_request_blob_sweep_skips_malformed_hash() {
        use gradient_entity::ids::{BuildRequestBlobId, ProjectId};

        let tmp = tempfile::tempdir().unwrap();
        let bad = gradient_entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            project: ProjectId::now_v7(),
            hash: vec![1, 2, 3],
            size: 1,
            created_at: now() - chrono::Duration::days(30),
            last_used_at: now() - chrono::Duration::days(30),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![bad]])
            .into_connection();
        let state = state_with_worker_db(tmp.path(), db);

        cleanup_unused_build_request_blobs(state).await.unwrap();
    }

    #[tokio::test]
    async fn upload_session_sweep_deletes_expired_undispatched() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 3,
            }])
            .into_connection();
        let state = state_with_worker_db(tmp.path(), db);

        cleanup_expired_upload_sessions(state).await.unwrap();
    }
}
