/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use futures::future::BoxFuture;
use object_store::{ObjectStore, PutPayload, path::Path as ObjectPath};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::warn;
use uuid::Uuid;

/// Abstraction for build log storage.
///
/// Logs are appended during a build (fast path) and finalized once when the build
/// reaches a terminal state. Backends that need to ship logs to remote storage
/// (e.g. S3) do that work in `finalize`.
pub trait LogStorage: Send + Sync + std::fmt::Debug {
    /// Append `text` to the log for `build_id`.
    fn append<'a>(&'a self, build_id: Uuid, text: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Read the full log for `build_id`. Returns an empty string when no log exists yet.
    fn read<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<String>>;

    /// Called once after the build reaches a terminal state. Default impl is a no-op;
    /// remote backends use this hook to upload the local file to object storage.
    fn finalize<'a>(&'a self, _build_id: Uuid) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Permanently delete the log for `build_id` from all backing stores.
    fn delete<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<()>>;
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

    pub fn log_path(&self, build_id: Uuid) -> PathBuf {
        self.logs_dir.join(format!("{}.log", build_id))
    }
}

impl LogStorage for FileLogStorage {
    fn append<'a>(&'a self, build_id: Uuid, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let path = self.log_path(build_id);
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            file.write_all(text.as_bytes()).await?;
            Ok(())
        })
    }

    fn read<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let path = self.log_path(build_id);
            match fs::read_to_string(&path).await {
                Ok(content) => Ok(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn delete<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let path = self.log_path(build_id);
            match fs::remove_file(&path).await {
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

    fn object_path(&self, build_id: Uuid) -> ObjectPath {
        ObjectPath::from(format!("{}logs/{}.log", self.prefix, build_id))
    }
}

impl LogStorage for S3LogStorage {
    fn append<'a>(&'a self, build_id: Uuid, text: &'a str) -> BoxFuture<'a, Result<()>> {
        self.local.append(build_id, text)
    }

    fn read<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<String>> {
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
                Err(object_store::Error::NotFound { .. }) => Ok(String::new()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn finalize<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<()>> {
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

    fn delete<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Err(e) = self.local.delete(build_id).await {
                warn!(error = %e, build_id = %build_id, "Failed to delete local build log");
            }
            match self.object_store.delete(&self.object_path(build_id)).await {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }
}
