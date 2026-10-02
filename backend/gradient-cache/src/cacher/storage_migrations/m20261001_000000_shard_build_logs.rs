/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::StorageMigration;
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use futures::{StreamExt as _, TryStreamExt as _};
use gradient_core::ServerState;
use gradient_storage::log_shard;
use gradient_types::ids::BuildAttemptId;
use object_store::{ObjectStore, path::Path as ObjectPath};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::BTreeSet;
use std::path::Path;
use tokio::fs;
use tracing::info;

pub(super) struct Migration;

impl StorageMigration for Migration {
    fn name(&self) -> &'static str {
        "m20261001_000000_shard_build_logs"
    }

    fn units(&self) -> Vec<String> {
        vec!["local".to_owned(), "s3".to_owned()]
    }

    fn migrate<'a>(&'a self, state: &'a ServerState, unit: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match unit {
                "local" => {
                    let logs = Path::new(&state.config.server.base_dir).join("logs");
                    let relocated = relocate_flat_entries(&logs).await?;
                    info!(relocated, "moved flat build logs into their shard");
                }
                _ if state.nar_storage.local_base().is_some() => {}
                _ => {
                    let store = state.nar_storage.inner();
                    let root = ObjectPath::from(format!("{}logs", state.nar_storage.prefix()));
                    let deleted = delete_flat_objects(store.as_ref(), &root).await?;
                    drop_chunk_index(state, &deleted).await?;
                    info!(deleted = deleted.len(), "deleted flat build logs from S3");
                }
            }
            Ok(())
        })
    }
}

fn attempt_of(entry: &str) -> Option<BuildAttemptId> {
    let stem = entry.strip_suffix(".log").unwrap_or(entry);
    stem.parse::<uuid::Uuid>().ok().map(BuildAttemptId::new)
}

async fn relocate_flat_entries(logs: &Path) -> Result<u64> {
    let mut entries = match fs::read_dir(logs).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut flat = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if let Some(name) = entry.file_name().to_str()
            && let Some(id) = attempt_of(name)
        {
            flat.push((id, name.to_owned()));
        }
    }

    for (attempt_id, name) in &flat {
        relocate_flat_entry(logs, *attempt_id, name).await?;
    }
    Ok(flat.len() as u64)
}

/// A live log already in the shard is keeping its order because the flat file is holding the
/// earlier lines. A chunk dir already in the shard was written later and is winning over the flat
/// one.
async fn relocate_flat_entry(logs: &Path, attempt_id: BuildAttemptId, name: &str) -> Result<()> {
    let shard = logs.join(log_shard(attempt_id));
    let from = logs.join(name);
    let to = shard.join(name);
    fs::create_dir_all(&shard).await?;
    if !fs::try_exists(&to).await? {
        fs::rename(&from, &to).await?;
    } else if name.ends_with(".log") {
        let mut merged = fs::read(&from).await?;
        merged.extend(fs::read(&to).await?);
        fs::write(&to, merged).await?;
        fs::remove_file(&from).await?;
    } else {
        fs::remove_dir_all(&from).await?;
    }
    Ok(())
}

async fn delete_flat_objects(
    store: &dyn ObjectStore,
    root: &ObjectPath,
) -> Result<Vec<BuildAttemptId>> {
    let top = store.list_with_delimiter(Some(root)).await?;
    let flat_objects = top.objects.into_iter().map(|meta| meta.location);
    let mut deleted = BTreeSet::new();
    let mut doomed = Vec::new();
    for location in flat_objects.chain(top.common_prefixes) {
        if let Some(id) = location.filename().and_then(attempt_of) {
            deleted.insert(id);
            doomed.push(location);
        }
    }

    for location in doomed {
        let objects = store
            .list(Some(&location))
            .map_ok(|meta| meta.location)
            .chain(futures::stream::once(async move { Ok(location) }))
            .boxed();
        let mut results = store.delete_stream(objects);
        while let Some(result) = results.next().await {
            match result {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(deleted.into_iter().collect())
}

async fn drop_chunk_index(state: &ServerState, deleted: &[BuildAttemptId]) -> Result<()> {
    gradient_db::for_each_chunk(deleted, |chunk| {
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
    use gradient_storage::{FileLogStorage, LogStorage, S3LogStorage};
    use object_store::{ObjectStoreExt as _, PutPayload, memory::InMemory};
    use std::sync::Arc;

    const SAMPLE: &str = "01890a5d-ac96-774b-bcce-b302099a8efe";

    #[tokio::test]
    async fn local_flat_entries_move_into_their_shard() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let logs = dir.path().join("logs");
        let id = BuildAttemptId::new(SAMPLE.parse().unwrap());
        fs::write(logs.join(format!("{SAMPLE}.log")), "early\n")
            .await
            .unwrap();
        fs::create_dir_all(logs.join(SAMPLE)).await.unwrap();
        fs::write(logs.join(SAMPLE).join("chunk_00000000.zst"), b"z")
            .await
            .unwrap();
        storage.append(id, "late\n").await.unwrap();

        assert_eq!(relocate_flat_entries(&logs).await.unwrap(), 2);

        assert_eq!(storage.read_inline(id).await.unwrap(), "early\nlate\n");
        assert_eq!(storage.read_chunk(id, 0).await.unwrap(), b"z");
        assert!(!logs.join(SAMPLE).exists());
        assert!(!logs.join(format!("{SAMPLE}.log")).exists());
    }

    #[tokio::test]
    async fn s3_flat_objects_are_deleted_and_shards_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemory::new());
        let s3 = S3LogStorage::new(
            FileLogStorage::new(dir.path()).await.unwrap(),
            store.clone(),
            "pre",
        );
        let legacy = BuildAttemptId::new(uuid::Uuid::now_v7());
        for key in [
            format!("pre/logs/{legacy}/chunk_00000000.zst"),
            format!("pre/logs/{legacy}/chunk_00000001.zst"),
            format!("pre/logs/{legacy}.log"),
        ] {
            store
                .put(&ObjectPath::from(key), PutPayload::from_static(b"old"))
                .await
                .unwrap();
        }
        let kept = BuildAttemptId::new(SAMPLE.parse().unwrap());
        s3.write_chunk(kept, 0, b"new").await.unwrap();

        let deleted = delete_flat_objects(store.as_ref(), &ObjectPath::from("pre/logs"))
            .await
            .unwrap();

        assert_eq!(deleted, vec![legacy]);
        let left: Vec<String> = store
            .list(Some(&ObjectPath::from("pre/logs")))
            .map_ok(|meta| meta.location.to_string())
            .try_collect()
            .await
            .unwrap();
        assert_eq!(
            left,
            vec![format!("pre/logs/fe/{SAMPLE}/chunk_00000000.zst")]
        );
    }
}
