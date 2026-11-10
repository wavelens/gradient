/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A concurrent `nix-collect-garbage` would otherwise delete build inputs or outputs before the
//! push. Leftover symlinks are coming from a crashed prior worker, and the keeper is purging them
//! at startup.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use gradient_util::store_path::strip_store_prefix;
use tokio::fs;
use tracing::{debug, warn};

use crate::nix::store::LocalNixStore;

#[derive(Clone)]
pub struct GcRootKeeper {
    inner: Arc<KeeperInner>,
}

struct KeeperInner {
    dir: Option<PathBuf>,
    store: Arc<LocalNixStore>,
    next_gc_root_symlink_number: AtomicU64,
}

impl GcRootKeeper {
    pub fn new(gcroots_dir: &str, store: Arc<LocalNixStore>) -> Self {
        let dir = if gcroots_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(gcroots_dir))
        };
        Self {
            inner: Arc::new(KeeperInner {
                dir,
                store,
                next_gc_root_symlink_number: AtomicU64::new(0),
            }),
        }
    }

    pub async fn purge_all(&self) -> Result<()> {
        let Some(dir) = self.inner.dir.as_ref() else {
            return Ok(());
        };
        fs::create_dir_all(dir)
            .await
            .with_context(|| format!("create gcroots dir {}", dir.display()))?;

        let mut entries = fs::read_dir(dir)
            .await
            .with_context(|| format!("read gcroots dir {}", dir.display()))?;
        let mut removed = 0u32;
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("iterate gcroots dir {}", dir.display()))?
        {
            let path = entry.path();
            if let Err(e) = fs::remove_file(&path).await {
                warn!(path = %path.display(), error = %e, "purge_all: skipping unreadable entry");
            } else {
                removed += 1;
            }
        }
        debug!(dir = %dir.display(), removed, "purged stale gcroots");
        Ok(())
    }

    fn new_gc_root_symlink_path(&self, store_path: &str) -> Option<PathBuf> {
        let dir = self.inner.dir.as_ref()?;
        let number = self
            .inner
            .next_gc_root_symlink_number
            .fetch_add(1, Ordering::Relaxed);
        Some(dir.join(format!("{}-{number}", strip_store_prefix(store_path))))
    }

    pub async fn add(&self, store_path: &str) -> GcRootHandle {
        let Some(symlink) = self.new_gc_root_symlink_path(store_path) else {
            return GcRootHandle::inert();
        };

        if let Err(e) = fs::symlink(store_path, &symlink).await {
            warn!(path = %store_path, error = %e, "gcroot: symlink create failed; build proceeds unpinned");
            return GcRootHandle::inert();
        }

        if let Err(e) = self.inner.store.add_indirect_root(&symlink).await {
            warn!(path = %store_path, error = %e, "gcroot: add_indirect_root failed; removing symlink");
            let _ = fs::remove_file(&symlink).await;
            return GcRootHandle::inert();
        }

        GcRootHandle {
            symlink: Some(symlink),
        }
    }
}

pub struct GcRootHandle {
    symlink: Option<PathBuf>,
}

impl GcRootHandle {
    fn inert() -> Self {
        Self { symlink: None }
    }
}

impl Drop for GcRootHandle {
    fn drop(&mut self) {
        if let Some(symlink) = self.symlink.take()
            && let Err(e) = std::fs::remove_file(&symlink)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(symlink = %symlink.display(), error = %e, "gcroot: failed to remove symlink on drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_keeper() -> GcRootKeeper {
        GcRootKeeper::new(
            "",
            Arc::new(LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap()),
        )
    }

    #[tokio::test]
    async fn disabled_keeper_purge_is_noop() {
        let keeper = disabled_keeper();
        keeper.purge_all().await.unwrap();
    }

    #[tokio::test]
    async fn disabled_keeper_add_returns_inert_handle() {
        let keeper = disabled_keeper();
        let handle = keeper.add("/nix/store/abc-foo").await;
        assert!(handle.symlink.is_none());
    }

    #[tokio::test]
    async fn purge_all_removes_existing_entries_and_creates_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("gcroots");
        tokio::fs::create_dir(&dir).await.unwrap();
        tokio::fs::write(dir.join("stale-1"), b"").await.unwrap();
        tokio::fs::symlink("/nix/store/abc-foo", dir.join("stale-2"))
            .await
            .unwrap();

        let keeper = GcRootKeeper::new(
            dir.to_str().unwrap(),
            Arc::new(LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap()),
        );
        keeper.purge_all().await.unwrap();

        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn purge_all_creates_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("not-yet-created");
        let keeper = GcRootKeeper::new(
            dir.to_str().unwrap(),
            Arc::new(LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap()),
        );
        keeper.purge_all().await.unwrap();
        assert!(dir.is_dir());
    }

    #[tokio::test]
    async fn drop_removes_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let symlink = tmp.path().join("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-foo");
        tokio::fs::symlink("/nix/store/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-foo", &symlink)
            .await
            .unwrap();
        let handle = GcRootHandle {
            symlink: Some(symlink.clone()),
        };
        drop(handle);
        assert!(symlink.symlink_metadata().is_err());
    }

    #[test]
    fn two_holders_of_one_store_path_get_their_own_symlinks() {
        let keeper = GcRootKeeper::new(
            "/var/empty/gcroots",
            Arc::new(LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap()),
        );

        let first = keeper.new_gc_root_symlink_path("/nix/store/abc-foo");
        let second = keeper.new_gc_root_symlink_path("/nix/store/abc-foo");

        assert_eq!(first, Some(PathBuf::from("/var/empty/gcroots/abc-foo-0")));
        assert_eq!(second, Some(PathBuf::from("/var/empty/gcroots/abc-foo-1")));
    }
}
