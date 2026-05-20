/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Indirect GC roots for active builds.
//!
//! Concurrent `nix-collect-garbage` on the worker would otherwise be free to
//! delete a derivation's inputs (or its just-built outputs before
//! compress+push uploads them). `GcRootKeeper` pins the .drv and each
//! realised output via harmonia's `add_indirect_root` for the duration of
//! the build job. Symlinks live under `gcroots_dir` (default
//! `/nix/var/nix/gcroots/gradient`) and are removed on handle drop. The
//! keeper purges leftovers at worker startup — anything still in the dir
//! came from a prior worker that crashed before its handles ran.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::fs;
use tracing::{debug, warn};

use crate::nix::store::LocalNixStore;

/// Manages the on-disk gcroots directory and hands out RAII handles that
/// register / release indirect GC roots through the local nix-daemon.
#[derive(Clone)]
pub struct GcRootKeeper {
    inner: Arc<KeeperInner>,
}

struct KeeperInner {
    dir: Option<PathBuf>,
    store: Arc<LocalNixStore>,
}

impl GcRootKeeper {
    /// Construct a keeper. An empty `gcroots_dir` disables the feature.
    pub fn new(gcroots_dir: &str, store: Arc<LocalNixStore>) -> Self {
        let dir = if gcroots_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(gcroots_dir))
        };
        Self {
            inner: Arc::new(KeeperInner { dir, store }),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.dir.is_some()
    }

    /// Remove every entry under the gcroots dir and recreate the dir if it
    /// doesn't exist. Stale leftovers came from a prior crashed worker.
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

    /// Add an indirect GC root for `store_path` (e.g.
    /// `/nix/store/<hash>-<name>` or `/nix/store/<hash>-<name>.drv`).
    /// The returned handle removes the symlink on drop.
    ///
    /// Returns an inert handle when the keeper is disabled. Errors creating
    /// the symlink or registering with the daemon are logged and the
    /// inert handle is returned — bookkeeping failures never fail a build.
    pub async fn add(&self, store_path: &str) -> GcRootHandle {
        let Some(dir) = self.inner.dir.as_ref() else {
            return GcRootHandle::inert();
        };
        let hash_name = store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(store_path);
        let symlink = dir.join(hash_name);

        if let Err(e) = create_symlink_idempotent(&symlink, store_path).await {
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

async fn create_symlink_idempotent(symlink: &Path, target: &str) -> Result<()> {
    match fs::symlink_metadata(symlink).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::symlink(target, symlink)
            .await
            .with_context(|| format!("symlink {} -> {target}", symlink.display())),
        Err(e) => Err(anyhow::anyhow!("stat {}: {e}", symlink.display())),
    }
}

/// RAII guard for a single indirect GC root. Removes its symlink on drop.
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
            Arc::new(
                LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap(),
            ),
        )
    }

    #[tokio::test]
    async fn disabled_keeper_purge_is_noop() {
        let keeper = disabled_keeper();
        keeper.purge_all().await.unwrap();
        assert!(!keeper.is_enabled());
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
            Arc::new(
                LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap(),
            ),
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
            Arc::new(
                LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).unwrap(),
            ),
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

    #[tokio::test]
    async fn create_symlink_idempotent_skips_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let symlink = tmp.path().join("link");
        tokio::fs::symlink("/nix/store/existing", &symlink)
            .await
            .unwrap();
        create_symlink_idempotent(&symlink, "/nix/store/other")
            .await
            .unwrap();
        let target = tokio::fs::read_link(&symlink).await.unwrap();
        assert_eq!(target, std::path::PathBuf::from("/nix/store/existing"));
    }
}
