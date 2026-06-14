/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::BuildId;
use anyhow::Result;
use futures::future::BoxFuture;
use object_store::{ObjectStore, ObjectStoreExt as _, PutPayload, path::Path as ObjectPath};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Abstraction for build log storage.
///
/// Logs are appended during a build (fast path) and finalized once when the build
/// reaches a terminal state. Backends that need to ship logs to remote storage
/// (e.g. S3) do that work in `finalize`.
pub trait LogStorage: Send + Sync + std::fmt::Debug {
    /// Append `text` to the log for `build_id`.
    fn append<'a>(&'a self, build_id: BuildId, text: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Read the full log for `build_id`. Returns an empty string when no log exists yet.
    fn read<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<String>>;

    /// Called once after the build reaches a terminal state. Default impl is a no-op;
    /// remote backends use this hook to upload the local file to object storage.
    fn finalize<'a>(&'a self, _build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Permanently delete the log for `build_id` from all backing stores.
    fn delete<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>>;

    /// Enumerate every `BuildId` that currently has a log in this backend.
    /// Used by the deep-GC sweep to find orphan logs.
    fn list_logs<'a>(&'a self) -> BoxFuture<'a, Result<Vec<BuildId>>>;

    /// Write one compressed log chunk object.
    fn write_chunk<'a>(
        &'a self,
        build_id: BuildId,
        index: u32,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<()>>;

    /// Read one compressed log chunk object's bytes.
    fn read_chunk<'a>(&'a self, build_id: BuildId, index: u32) -> BoxFuture<'a, Result<Vec<u8>>>;

    /// Delete all chunk objects for `build_id`.
    fn delete_chunks<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>>;

    /// Drop only the inline (uncompressed) log, keeping any chunk objects.
    /// Called after `finalize` has written the chunked representation so the
    /// compressed chunks become the sole at-rest copy. Default is a no-op.
    fn delete_inline_log<'a>(&'a self, _build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Concatenate the decompressed chunk objects in order. Used as a fallback
    /// by `read` once the inline log has been dropped. Stops at the first
    /// missing chunk index.
    fn reassemble_chunks<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut out = String::new();
            let mut index = 0u32;
            while let Ok(raw) = self.read_chunk(build_id, index).await {
                let bytes = zstd::stream::decode_all(&raw[..])?;
                out.push_str(&String::from_utf8_lossy(&bytes));
                index += 1;
            }
            Ok(out)
        })
    }
}

#[derive(Debug)]
pub struct FileLogStorage {
    logs_dir: PathBuf,
}

impl FileLogStorage {
    pub async fn new(base_path: &Path) -> Result<Self> {
        let logs_dir = base_path.join("logs");
        fs::create_dir_all(&logs_dir).await?;
        let storage = Self { logs_dir };
        storage.shard_existing_logs().await?;
        Ok(storage)
    }

    /// Two-char shard derived from the final UUID byte (e.g. `…8814fe` → `fe`),
    /// fanning logs across 256 subfolders instead of one flat directory.
    fn shard(build_id: BuildId) -> String {
        format!("{:02x}", build_id.into_inner().as_bytes()[15])
    }

    fn shard_dir(&self, build_id: BuildId) -> PathBuf {
        self.logs_dir.join(Self::shard(build_id))
    }

    pub fn log_path(&self, build_id: BuildId) -> PathBuf {
        self.shard_dir(build_id).join(format!("{}.log", build_id))
    }

    fn chunk_dir(&self, build_id: BuildId) -> PathBuf {
        self.shard_dir(build_id).join(build_id.to_string())
    }

    fn chunk_path(&self, build_id: BuildId, index: u32) -> PathBuf {
        self.chunk_dir(build_id)
            .join(format!("chunk_{:08}.zst", index))
    }

    /// One-time idempotent relocation of pre-sharding flat entries
    /// (`<uuid>.log` files and bare `<uuid>` chunk dirs) into their shard
    /// subfolder. Already-sharded two-char dirs fail the UUID parse and are skipped.
    async fn shard_existing_logs(&self) -> Result<()> {
        let mut flat: Vec<(BuildId, String)> = Vec::new();
        let mut entries = fs::read_dir(&self.logs_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(s) = name.to_str() else { continue };
            let stem = s.strip_suffix(".log").unwrap_or(s);
            if let Ok(id) = stem.parse::<uuid::Uuid>() {
                flat.push((BuildId::new(id), s.to_owned()));
            }
        }

        for (build_id, name) in flat {
            let dest = self.shard_dir(build_id);
            if let Err(e) = async {
                fs::create_dir_all(&dest).await?;
                fs::rename(self.logs_dir.join(&name), dest.join(&name)).await
            }
            .await
            {
                warn!(error = %e, entry = %name, "failed to relocate log into shard subfolder");
            }
        }

        Ok(())
    }
}

impl LogStorage for FileLogStorage {
    fn append<'a>(&'a self, build_id: BuildId, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let path = self.log_path(build_id);
            fs::create_dir_all(self.shard_dir(build_id)).await?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            file.write_all(text.as_bytes()).await?;
            Ok(())
        })
    }

    fn read<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let path = self.log_path(build_id);
            match fs::read_to_string(&path).await {
                Ok(content) if !content.is_empty() => Ok(content),
                Ok(_) => self.reassemble_chunks(build_id).await,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.reassemble_chunks(build_id).await
                }
                Err(e) => Err(e.into()),
            }
        })
    }

    fn delete<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.delete_chunks(build_id).await.ok();
            let path = self.log_path(build_id);
            match fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn list_logs<'a>(&'a self) -> BoxFuture<'a, Result<Vec<BuildId>>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let mut shards = match fs::read_dir(&self.logs_dir).await {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
                Err(e) => return Err(e.into()),
            };
            while let Some(shard) = shards.next_entry().await? {
                if !shard.file_type().await?.is_dir() {
                    continue;
                }

                let mut entries = fs::read_dir(shard.path()).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let name = entry.file_name();
                    let Some(s) = name.to_str() else { continue };
                    let Some(stem) = s.strip_suffix(".log") else {
                        continue;
                    };
                    if let Ok(id) = stem.parse::<uuid::Uuid>() {
                        out.push(BuildId::new(id));
                    }
                }
            }
            Ok(out)
        })
    }

    fn write_chunk<'a>(
        &'a self,
        build_id: BuildId,
        index: u32,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            fs::create_dir_all(self.chunk_dir(build_id)).await?;
            fs::write(self.chunk_path(build_id, index), bytes).await?;
            Ok(())
        })
    }

    fn read_chunk<'a>(&'a self, build_id: BuildId, index: u32) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move { Ok(fs::read(self.chunk_path(build_id, index)).await?) })
    }

    fn delete_chunks<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match fs::remove_dir_all(self.chunk_dir(build_id)).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn delete_inline_log<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match fs::remove_file(self.log_path(build_id)).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }
}

/// Log storage that keeps a local copy for fast appends and uploads the final
/// log to S3-compatible object storage when the build reaches a terminal state.
///
/// Reads always try the local file first; if missing they fall back to S3.
/// `delete` removes both copies.
pub struct S3LogStorage {
    local: FileLogStorage,
    object_store: Arc<dyn ObjectStore>,
    prefix: String,
}

impl std::fmt::Debug for S3LogStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3LogStorage")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl S3LogStorage {
    pub fn new(local: FileLogStorage, object_store: Arc<dyn ObjectStore>, prefix: &str) -> Self {
        let normalized_prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{}/", prefix)
        };
        Self {
            local,
            object_store,
            prefix: normalized_prefix,
        }
    }

    fn object_path(&self, build_id: BuildId) -> ObjectPath {
        ObjectPath::from(format!("{}logs/{}.log", self.prefix, build_id))
    }

    fn chunk_object_path(&self, build_id: BuildId, index: u32) -> ObjectPath {
        ObjectPath::from(format!(
            "{}logs/{}/chunk_{:08}.zst",
            self.prefix, build_id, index
        ))
    }
}

impl LogStorage for S3LogStorage {
    fn append<'a>(&'a self, build_id: BuildId, text: &'a str) -> BoxFuture<'a, Result<()>> {
        self.local.append(build_id, text)
    }

    fn read<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let local = self.local.read(build_id).await?;
            if !local.is_empty() {
                return Ok(local);
            }
            match self.object_store.get(&self.object_path(build_id)).await {
                Ok(result) => {
                    let bytes = result.bytes().await?;
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                }
                Err(object_store::Error::NotFound { .. }) => {
                    self.reassemble_chunks(build_id).await
                }
                Err(e) => Err(e.into()),
            }
        })
    }

    fn finalize<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let path = self.local.log_path(build_id);
            let data = match fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            };
            self.object_store
                .put(&self.object_path(build_id), PutPayload::from(data))
                .await?;
            // Local copy is kept as a read cache; the existing GC paths remove it
            // through `LogStorage::delete` when the evaluation is GC'd.
            Ok(())
        })
    }

    fn delete<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.delete_chunks(build_id).await.ok();
            if let Err(e) = self.local.delete(build_id).await {
                warn!(error = %e, build_id = %build_id, "Failed to delete local build log");
            }
            match self.object_store.delete(&self.object_path(build_id)).await {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn list_logs<'a>(&'a self) -> BoxFuture<'a, Result<Vec<BuildId>>> {
        Box::pin(async move {
            use futures::StreamExt as _;
            let mut local = self.local.list_logs().await?;
            let prefix = ObjectPath::from(format!("{}logs", self.prefix));
            let mut stream = self.object_store.list(Some(&prefix));
            let mut seen: std::collections::HashSet<BuildId> = local.iter().copied().collect();
            while let Some(item) = stream.next().await {
                let meta = item?;
                let p = meta.location.to_string();
                let Some(name) = p.split('/').next_back() else {
                    continue;
                };
                let Some(stem) = name.strip_suffix(".log") else {
                    continue;
                };
                if let Ok(id) = stem.parse::<uuid::Uuid>() {
                    let bid = BuildId::new(id);
                    if seen.insert(bid) {
                        local.push(bid);
                    }
                }
            }
            Ok(local)
        })
    }

    fn write_chunk<'a>(
        &'a self,
        build_id: BuildId,
        index: u32,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.local.write_chunk(build_id, index, bytes).await?;
            self.object_store
                .put(
                    &self.chunk_object_path(build_id, index),
                    PutPayload::from(bytes.to_vec()),
                )
                .await?;
            Ok(())
        })
    }

    fn read_chunk<'a>(&'a self, build_id: BuildId, index: u32) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            if let Ok(bytes) = self.local.read_chunk(build_id, index).await {
                return Ok(bytes);
            }
            let result = self
                .object_store
                .get(&self.chunk_object_path(build_id, index))
                .await?;
            Ok(result.bytes().await?.to_vec())
        })
    }

    fn delete_chunks<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            use futures::StreamExt as _;
            if let Err(e) = self.local.delete_chunks(build_id).await {
                warn!(error = %e, build_id = %build_id, "Failed to delete local log chunks");
            }
            let prefix = ObjectPath::from(format!("{}logs/{}", self.prefix, build_id));
            let mut stream = self.object_store.list(Some(&prefix));
            while let Some(item) = stream.next().await {
                let meta = item?;
                let _ = self.object_store.delete(&meta.location).await;
            }
            Ok(())
        })
    }

    fn delete_inline_log<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _ = self.local.delete_inline_log(build_id).await;
            match self.object_store.delete(&self.object_path(build_id)).await {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use gradient_types::ids::BuildId;

    #[tokio::test]
    async fn write_read_delete_chunk_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = BuildId::new(uuid::Uuid::new_v4());
        storage.write_chunk(id, 0, b"hello").await.unwrap();
        storage.write_chunk(id, 1, b"world").await.unwrap();
        assert_eq!(storage.read_chunk(id, 0).await.unwrap(), b"hello");
        assert_eq!(storage.read_chunk(id, 1).await.unwrap(), b"world");
        storage.delete_chunks(id).await.unwrap();
        assert!(storage.read_chunk(id, 0).await.is_err());
    }
}

#[cfg(test)]
mod shard_tests {
    use super::*;
    use gradient_types::ids::BuildId;

    const SAMPLE: &str = "019e884e-6430-7d83-86a1-3d0e6d8814fe";

    fn sample_id() -> BuildId {
        BuildId::new(SAMPLE.parse().unwrap())
    }

    #[tokio::test]
    async fn log_lives_in_two_char_shard_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = sample_id();
        storage.append(id, "hello").await.unwrap();

        let expected = dir.path().join("logs").join("fe").join(format!("{SAMPLE}.log"));
        assert!(expected.exists(), "log not sharded to logs/fe/{SAMPLE}.log");
        assert_eq!(storage.read(id).await.unwrap(), "hello");
        assert_eq!(storage.list_logs().await.unwrap(), vec![id]);
    }

    #[tokio::test]
    async fn startup_migration_relocates_flat_entries() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let chunk_dir = logs.join(SAMPLE);
        fs::create_dir_all(&chunk_dir).await.unwrap();
        fs::write(logs.join(format!("{SAMPLE}.log")), "legacy").await.unwrap();
        fs::write(chunk_dir.join("chunk_00000000.zst"), b"z").await.unwrap();

        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = sample_id();

        assert!(!logs.join(format!("{SAMPLE}.log")).exists());
        assert!(logs.join("fe").join(format!("{SAMPLE}.log")).exists());
        assert!(logs.join("fe").join(SAMPLE).join("chunk_00000000.zst").exists());
        assert_eq!(storage.read(id).await.unwrap(), "legacy");
        assert_eq!(storage.list_logs().await.unwrap(), vec![id]);
    }
}
