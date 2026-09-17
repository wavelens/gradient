/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Relayed NARs the S3 backend still owes to object storage. A file lives here
//! from the relay commit until the uploader confirms the row; together with the
//! `cached_path` row whose `confirmed` is false it is the upload queue, durable
//! across a restart with no table of its own.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use tokio::sync::Notify;

const SUFFIX: &str = ".nar.zst";

#[derive(Debug)]
pub struct StagedNars {
    root: PathBuf,
    wake: Notify,
}

impl StagedNars {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create staged NAR dir {}", root.display()))?;

        Ok(Self {
            root,
            wake: Notify::new(),
        })
    }

    pub fn path(&self, hash: &str) -> PathBuf {
        self.root.join(format!("{hash}{SUFFIX}"))
    }

    /// Move a verified claim under `hash`: a rename on the same device, a copy
    /// otherwise. The source is gone on success.
    pub async fn adopt(&self, hash: &str, from: &Path) -> Result<PathBuf> {
        let dest = self.path(hash);
        match tokio::fs::rename(from, &dest).await {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
            Err(e) => return Err(e).context("rename claim into nar-staged"),
        }

        tokio::fs::copy(from, &dest)
            .await
            .context("copy claim into nar-staged")?;
        tokio::fs::remove_file(from)
            .await
            .context("remove claim after copy")?;

        Ok(dest)
    }

    pub async fn exists(&self, hash: &str) -> bool {
        tokio::fs::metadata(self.path(hash)).await.is_ok()
    }

    /// The staged file and its length, `None` when nothing is staged for `hash`.
    pub async fn open(&self, hash: &str) -> Result<Option<(u64, tokio::fs::File)>> {
        let file = match tokio::fs::File::open(self.path(hash)).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("open staged NAR"),
        };

        let len = file.metadata().await.context("stat staged NAR")?.len();

        Ok(Some((len, file)))
    }

    pub async fn remove(&self, hash: &str) -> Result<()> {
        match tokio::fs::remove_file(self.path(hash)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("remove staged NAR"),
        }
    }

    /// Every staged hash with the file's modification time.
    pub async fn list(&self) -> Result<Vec<(String, SystemTime)>> {
        let mut out = Vec::new();
        let mut dir = tokio::fs::read_dir(&self.root)
            .await
            .context("read nar-staged")?;
        while let Some(entry) = dir.next_entry().await.context("read nar-staged entry")? {
            let name = entry.file_name();
            let Some(hash) = name.to_str().and_then(|n| n.strip_suffix(SUFFIX)) else {
                continue;
            };

            let modified = entry
                .metadata()
                .await
                .context("stat staged NAR")?
                .modified()?;
            out.push((hash.to_owned(), modified));
        }

        Ok(out)
    }

    /// Tell the uploader a file is waiting.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Resolves on the next [`Self::wake`], or at once for one already stored.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::StagedNars;
    use tempfile::TempDir;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    async fn claim(dir: &TempDir, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join("claim");
        tokio::fs::write(&path, bytes).await.unwrap();
        path
    }

    #[tokio::test]
    async fn adopt_moves_the_claim_under_its_hash() {
        let dir = TempDir::new().unwrap();
        let staged = StagedNars::new(dir.path().join("nar-staged")).unwrap();
        let from = claim(&dir, b"nar bytes").await;

        let dest = staged.adopt(HASH, &from).await.unwrap();

        assert!(!from.exists(), "the claim is gone");
        assert_eq!(dest, staged.path(HASH));
        assert!(staged.exists(HASH).await);
        let (len, _) = staged.open(HASH).await.unwrap().expect("staged");
        assert_eq!(len, 9);
    }

    #[tokio::test]
    async fn remove_is_idempotent_and_list_names_every_hash() {
        let dir = TempDir::new().unwrap();
        let staged = StagedNars::new(dir.path().join("nar-staged")).unwrap();
        let from = claim(&dir, b"x").await;
        staged.adopt(HASH, &from).await.unwrap();

        let listed: Vec<String> = staged
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(listed, vec![HASH.to_owned()]);

        staged.remove(HASH).await.unwrap();
        staged.remove(HASH).await.unwrap();
        assert!(!staged.exists(HASH).await);
        assert!(staged.open(HASH).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_wake_before_anyone_waits_is_not_lost() {
        let dir = TempDir::new().unwrap();
        let staged = StagedNars::new(dir.path()).unwrap();
        staged.wake();
        tokio::time::timeout(std::time::Duration::from_secs(1), staged.woken())
            .await
            .expect("the stored permit resolves the next wait");
    }
}
