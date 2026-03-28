/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use futures::future::BoxFuture;
use std::path::{Path, PathBuf};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Abstraction for build log storage.
///
/// The current implementation writes logs to the local filesystem.
/// To switch to S3 (or any other backend), implement this trait and
/// update `ServerState::log_storage` in `core/src/lib.rs`.
pub trait LogStorage: Send + Sync + std::fmt::Debug {
    /// Append `text` to the log for `build_id`.
    fn append<'a>(&'a self, build_id: Uuid, text: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Read the full log for `build_id`. Returns an empty string when no log exists yet.
    fn read<'a>(&'a self, build_id: Uuid) -> BoxFuture<'a, Result<String>>;
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

    fn log_path(&self, build_id: Uuid) -> PathBuf {
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
}
