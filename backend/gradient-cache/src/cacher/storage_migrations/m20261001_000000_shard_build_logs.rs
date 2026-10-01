/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Retire the pre-shard flat log layout (`logs/<attempt>.log`, `logs/<attempt>/`):
//! local entries move into their shard, S3 entries are deleted, and so is the
//! `build_log_chunk` index of every deleted log, which would index nothing.

use super::StorageMigration;
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::info;

pub(super) struct Migration;

impl StorageMigration for Migration {
    fn name(&self) -> &'static str {
        "m20261001_000000_shard_build_logs"
    }

    fn units(&self) -> Vec<String> {
        vec!["logs".to_owned()]
    }

    fn migrate<'a>(&'a self, state: &'a ServerState, _unit: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let cleanup = state
                .log_storage
                .clean_legacy_layout()
                .await
                .context("clean_legacy_layout")?;
            gradient_db::for_each_chunk(&cleanup.deleted, |chunk| {
                gradient_entity::build_log_chunk::Entity::delete_many()
                    .filter(gradient_entity::build_log_chunk::Column::BuildAttempt.is_in(chunk))
                    .exec(&state.worker_db)
            })
            .await
            .context("drop the chunk index of deleted legacy logs")?;

            info!(
                relocated = cleanup.relocated,
                deleted = cleanup.deleted.len(),
                "retired the flat build log layout"
            );
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cacher::test_support::test_server_state_with_log;
    use gradient_storage::{FileLogStorage, LogStorage, NarStore, S3LogStorage};
    use gradient_types::ids::BuildAttemptId;
    use object_store::{
        ObjectStore as _, ObjectStoreExt as _, PutPayload, memory::InMemory,
        path::Path as ObjectPath,
    };
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;

    #[tokio::test]
    async fn deletes_flat_s3_logs_and_their_chunk_index() {
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
        let state = test_server_state_with_log(nar, log, db, |_| {});

        Migration.migrate(&state, "logs").await.unwrap();

        assert!(
            store
                .list_with_delimiter(Some(&ObjectPath::from("logs")))
                .await
                .unwrap()
                .common_prefixes
                .is_empty()
        );
    }
}
