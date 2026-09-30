/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use futures::future::BoxFuture;
use gradient_types::ids::BuildAttemptId;
use object_store::{ObjectStore, ObjectStoreExt as _, PutPayload, path::Path as ObjectPath};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

/// Abstraction for build log storage.
///
/// Logs are appended to an inline copy while a build runs; once the build is
/// terminal the log is split into compressed chunks and the inline copy dropped.
pub trait LogStorage: Send + Sync + std::fmt::Debug {
    /// Append `text` to the log for `attempt_id`.
    fn append<'a>(&'a self, attempt_id: BuildAttemptId, text: &'a str)
    -> BoxFuture<'a, Result<()>>;

    /// Read the full log for `attempt_id`. Returns an empty string when no log exists yet.
    fn read<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<String>>;

    /// Read only the inline (not yet chunked) log; empty when there is none.
    /// Defaults to `read` for backends without a separate chunked copy.
    fn read_inline<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<String>> {
        self.read(attempt_id)
    }

    /// Permanently delete the log for `attempt_id` from all backing stores.
    fn delete<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>>;

    /// Enumerate every `BuildAttemptId` that currently has a log in this backend,
    /// inline or chunked. Used by the deep-GC sweep to find orphan logs.
    fn list_logs<'a>(&'a self) -> BoxFuture<'a, Result<Vec<BuildAttemptId>>>;

    /// Write one compressed log chunk object.
    fn write_chunk<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        index: u32,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<()>>;

    /// Read one compressed log chunk object's bytes.
    fn read_chunk<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        index: u32,
    ) -> BoxFuture<'a, Result<Vec<u8>>>;

    /// Delete all chunk objects for `attempt_id`.
    fn delete_chunks<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>>;

    /// Drop only the inline (uncompressed) log, keeping any chunk objects.
    /// Called once the chunked representation is written, so the compressed
    /// chunks become the sole at-rest copy. Default is a no-op.
    fn delete_inline_log<'a>(&'a self, _attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Concatenate the decompressed chunk objects in order. Used as a fallback
    /// by `read` once the inline log has been dropped. Stops at the first
    /// missing chunk index.
    fn reassemble_chunks<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut out = String::new();
            let mut index = 0u32;
            while let Ok(raw) = self.read_chunk(attempt_id, index).await {
                let bytes = zstd::stream::decode_all(&raw[..])?;
                out.push_str(&String::from_utf8_lossy(&bytes));
                index += 1;
            }
            Ok(out)
        })
    }
}

/// Keys below `logs/`, shared by every backend. The shard is the last UUID
/// byte (the random tail of a v7 id), fanning logs across 256 subfolders:
/// `<xx>/<uuid>.log` inline, `<xx>/<uuid>/chunk_<n>.zst` chunked.
mod layout {
    use gradient_types::ids::BuildAttemptId;

    pub fn shard(attempt_id: BuildAttemptId) -> String {
        format!("{:02x}", attempt_id.into_inner().as_bytes()[15])
    }

    pub fn inline_key(attempt_id: BuildAttemptId) -> String {
        format!("{}/{attempt_id}.log", shard(attempt_id))
    }

    pub fn chunk_dir_key(attempt_id: BuildAttemptId) -> String {
        format!("{}/{attempt_id}", shard(attempt_id))
    }

    pub fn chunk_key(attempt_id: BuildAttemptId, index: u32) -> String {
        format!("{}/chunk_{index:08}.zst", chunk_dir_key(attempt_id))
    }

    /// The attempt owning a shard entry: an inline `<uuid>.log` or a chunk dir `<uuid>`.
    pub fn attempt_of(entry: &str) -> Option<BuildAttemptId> {
        let stem = entry.strip_suffix(".log").unwrap_or(entry);
        stem.parse::<uuid::Uuid>().ok().map(BuildAttemptId::new)
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
        Ok(Self { logs_dir })
    }

    pub fn log_path(&self, attempt_id: BuildAttemptId) -> PathBuf {
        self.logs_dir.join(layout::inline_key(attempt_id))
    }

    fn chunk_dir(&self, attempt_id: BuildAttemptId) -> PathBuf {
        self.logs_dir.join(layout::chunk_dir_key(attempt_id))
    }

    fn chunk_path(&self, attempt_id: BuildAttemptId, index: u32) -> PathBuf {
        self.logs_dir.join(layout::chunk_key(attempt_id, index))
    }
}

async fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

impl LogStorage for FileLogStorage {
    fn append<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let path = self.log_path(attempt_id);
            fs::create_dir_all(self.logs_dir.join(layout::shard(attempt_id))).await?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            file.write_all(text.as_bytes()).await?;
            // tokio buffers writes and submits the syscall to a blocking task;
            // without flushing, the in-flight write is detached on drop and a
            // read-after-write races it, observing an empty file.
            file.flush().await?;
            Ok(())
        })
    }

    fn read<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let inline = self.read_inline(attempt_id).await?;
            if !inline.is_empty() {
                return Ok(inline);
            }
            self.reassemble_chunks(attempt_id).await
        })
    }

    fn read_inline<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            match fs::read_to_string(self.log_path(attempt_id)).await {
                Ok(content) => Ok(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn delete<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chunks = self.delete_chunks(attempt_id).await;
            let inline = remove_file_if_present(&self.log_path(attempt_id)).await;
            chunks.and(inline)
        })
    }

    fn list_logs<'a>(&'a self) -> BoxFuture<'a, Result<Vec<BuildAttemptId>>> {
        Box::pin(async move {
            let mut out = BTreeSet::new();
            let mut shards = match fs::read_dir(&self.logs_dir).await {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(e.into()),
            };
            while let Some(shard) = shards.next_entry().await? {
                if !shard.file_type().await?.is_dir() {
                    continue;
                }

                let mut entries = fs::read_dir(shard.path()).await?;
                while let Some(entry) = entries.next_entry().await? {
                    if let Some(id) = entry.file_name().to_str().and_then(layout::attempt_of) {
                        out.insert(id);
                    }
                }
            }
            Ok(out.into_iter().collect())
        })
    }

    fn write_chunk<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        index: u32,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            fs::create_dir_all(self.chunk_dir(attempt_id)).await?;
            fs::write(self.chunk_path(attempt_id, index), bytes).await?;
            Ok(())
        })
    }

    fn read_chunk<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        index: u32,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move { Ok(fs::read(self.chunk_path(attempt_id, index)).await?) })
    }

    fn delete_chunks<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match fs::remove_dir_all(self.chunk_dir(attempt_id)).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn delete_inline_log<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { remove_file_if_present(&self.log_path(attempt_id)).await })
    }
}

/// Log storage that appends the live log to a local file (S3 has no efficient
/// append) and writes the finalized chunks only to S3-compatible object
/// storage, so an S3 backend keeps no build logs on local disk at rest. Reads
/// serve the live local file while a build runs, then the S3 chunks.
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
        Self {
            local,
            object_store,
            prefix: crate::layout::normalize_prefix(prefix),
        }
    }

    fn logs_root(&self) -> ObjectPath {
        ObjectPath::from(format!("{}logs", self.prefix))
    }

    fn object_path(&self, key: &str) -> ObjectPath {
        ObjectPath::from(format!("{}logs/{key}", self.prefix))
    }
}

impl LogStorage for S3LogStorage {
    fn append<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        self.local.append(attempt_id, text)
    }

    fn read<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let inline = self.read_inline(attempt_id).await?;
            if !inline.is_empty() {
                return Ok(inline);
            }
            self.reassemble_chunks(attempt_id).await
        })
    }

    fn read_inline<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<String>> {
        self.local.read_inline(attempt_id)
    }

    fn delete<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let local = self.local.delete(attempt_id).await;
            let remote = self.delete_chunks(attempt_id).await;
            local.and(remote)
        })
    }

    fn list_logs<'a>(&'a self) -> BoxFuture<'a, Result<Vec<BuildAttemptId>>> {
        Box::pin(async move {
            use futures::StreamExt as _;
            let mut out: BTreeSet<BuildAttemptId> =
                self.local.list_logs().await?.into_iter().collect();
            let root = self.logs_root();
            let mut stream = self.object_store.list(Some(&root));
            while let Some(item) = stream.next().await {
                let location = item?.location;
                let entry = location
                    .prefix_match(&root)
                    .and_then(|mut parts| parts.nth(1));
                if let Some(id) = entry.and_then(|p| layout::attempt_of(p.as_ref())) {
                    out.insert(id);
                }
            }
            Ok(out.into_iter().collect())
        })
    }

    fn write_chunk<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        index: u32,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let budget = crate::nar::single_write_budget(bytes.len());
            crate::nar::bounded(
                self.object_store.put(
                    &self.object_path(&layout::chunk_key(attempt_id, index)),
                    PutPayload::from(bytes.to_vec()),
                ),
                budget,
                "upload build-log chunk",
            )
            .await?;
            Ok(())
        })
    }

    fn read_chunk<'a>(
        &'a self,
        attempt_id: BuildAttemptId,
        index: u32,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            let result = self
                .object_store
                .get(&self.object_path(&layout::chunk_key(attempt_id, index)))
                .await?;
            Ok(result.bytes().await?.to_vec())
        })
    }

    fn delete_chunks<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            use futures::{StreamExt as _, TryStreamExt as _};
            let dir = self.object_path(&layout::chunk_dir_key(attempt_id));
            let locations = self
                .object_store
                .list(Some(&dir))
                .map_ok(|meta| meta.location)
                .boxed();
            let mut deleted = self.object_store.delete_stream(locations);
            while let Some(result) = deleted.next().await {
                match result {
                    Ok(_) | Err(object_store::Error::NotFound { .. }) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        })
    }

    fn delete_inline_log<'a>(&'a self, attempt_id: BuildAttemptId) -> BoxFuture<'a, Result<()>> {
        self.local.delete_inline_log(attempt_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    const SAMPLE: &str = "019e884e-6430-7d83-86a1-3d0e6d8814fe";

    fn sample_id() -> BuildAttemptId {
        BuildAttemptId::new(SAMPLE.parse().unwrap())
    }

    async fn s3_storage(dir: &Path) -> (S3LogStorage, Arc<InMemory>) {
        let store = Arc::new(InMemory::new());
        let local = FileLogStorage::new(dir).await.unwrap();
        (S3LogStorage::new(local, store.clone(), "pre"), store)
    }

    #[tokio::test]
    async fn write_read_delete_chunk_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = BuildAttemptId::new(uuid::Uuid::now_v7());
        storage.write_chunk(id, 0, b"hello").await.unwrap();
        storage.write_chunk(id, 1, b"world").await.unwrap();
        assert_eq!(storage.read_chunk(id, 0).await.unwrap(), b"hello");
        assert_eq!(storage.read_chunk(id, 1).await.unwrap(), b"world");
        storage.delete_chunks(id).await.unwrap();
        assert!(storage.read_chunk(id, 0).await.is_err());
    }

    #[tokio::test]
    async fn file_log_and_chunks_live_in_the_last_byte_shard() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let id = sample_id();
        storage.append(id, "hello").await.unwrap();
        storage.write_chunk(id, 0, b"z").await.unwrap();

        let shard = dir.path().join("logs").join("fe");
        assert!(shard.join(format!("{SAMPLE}.log")).exists());
        assert!(shard.join(SAMPLE).join("chunk_00000000.zst").exists());
        assert_eq!(storage.read(id).await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn file_list_logs_reports_chunk_only_logs_once() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FileLogStorage::new(dir.path()).await.unwrap();
        let live = BuildAttemptId::new(uuid::Uuid::now_v7());
        let finalized = sample_id();
        storage.append(live, "running").await.unwrap();
        storage.append(finalized, "done").await.unwrap();
        storage.write_chunk(finalized, 0, b"z").await.unwrap();
        storage.delete_inline_log(finalized).await.unwrap();
        storage.write_chunk(live, 0, b"z").await.unwrap();

        let mut listed = storage.list_logs().await.unwrap();
        listed.sort();
        let mut expected = vec![live, finalized];
        expected.sort();
        assert_eq!(listed, expected);

        storage.delete(finalized).await.unwrap();
        assert_eq!(storage.list_logs().await.unwrap(), vec![live]);
    }

    #[tokio::test]
    async fn s3_chunks_are_sharded_and_not_cached_on_local_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (s3, store) = s3_storage(dir.path()).await;
        let id = sample_id();

        s3.write_chunk(id, 0, b"hello").await.unwrap();

        let key = ObjectPath::from(format!("pre/logs/fe/{SAMPLE}/chunk_00000000.zst"));
        assert!(store.head(&key).await.is_ok(), "chunk not at {key}");
        assert_eq!(s3.read_chunk(id, 0).await.unwrap(), b"hello");
        assert!(
            s3.local.read_chunk(id, 0).await.is_err(),
            "S3 backend must not write chunks to local disk"
        );
    }

    #[tokio::test]
    async fn s3_list_logs_reports_finalized_logs_and_delete_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let (s3, _store) = s3_storage(dir.path()).await;
        let live = BuildAttemptId::new(uuid::Uuid::now_v7());
        let finalized = sample_id();
        s3.append(live, "running").await.unwrap();
        s3.write_chunk(finalized, 0, b"a").await.unwrap();
        s3.write_chunk(finalized, 1, b"b").await.unwrap();

        let mut listed = s3.list_logs().await.unwrap();
        listed.sort();
        let mut expected = vec![live, finalized];
        expected.sort();
        assert_eq!(listed, expected);

        s3.delete(finalized).await.unwrap();
        assert_eq!(s3.list_logs().await.unwrap(), vec![live]);
    }
}
