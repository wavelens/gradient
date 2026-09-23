/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::journal::{Journal, Violation};
use futures::StreamExt as _;
use harmonia_file_nar::archive::{NarWriteError, parse_nar, restore};
use harmonia_store_content_address::ContentAddress;
use harmonia_store_db::{OpenMode, StoreDb};
use harmonia_store_path::{StoreDir, StorePath, StorePathSet};
use harmonia_store_path_info::{NarHash, UnkeyedValidPathInfo};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZero;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Seeded,
    Added,
    Evaluated,
    Built,
    Imported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registered {
    New,
    AlreadyValid,
}

struct Entry {
    info: UnkeyedValidPathInfo,
    origin: Origin,
}

// The base DB (the VM image's db.sqlite) is only ever queried; its paths are masked, never deleted.
pub struct MockStore {
    root: PathBuf,
    store_dir: StoreDir,
    base: Option<Mutex<StoreDb>>,
    overlay: RwLock<BTreeMap<StorePath, Entry>>,
    forgotten: RwLock<BTreeSet<StorePath>>,
    registering: tokio::sync::Mutex<()>,
    journal: Arc<Journal>,
}

impl MockStore {
    pub fn open(
        root: PathBuf,
        base_db: Option<&Path>,
        journal: Arc<Journal>,
    ) -> anyhow::Result<Self> {
        let base = base_db
            .filter(|p| p.exists())
            .map(|p| StoreDb::open(p, OpenMode::ReadOnly).map(Mutex::new))
            .transpose()?;
        Ok(Self {
            root,
            store_dir: StoreDir::default(),
            base,
            overlay: RwLock::default(),
            forgotten: RwLock::default(),
            registering: tokio::sync::Mutex::new(()),
            journal,
        })
    }

    pub fn real_path(&self, p: &StorePath) -> PathBuf {
        self.root
            .join(self.store_dir.to_string().trim_start_matches('/'))
            .join(p.to_string())
    }

    pub fn info(&self, p: &StorePath) -> anyhow::Result<Option<UnkeyedValidPathInfo>> {
        if let Some(entry) = self.overlay.read().expect("overlay").get(p) {
            return Ok(Some(entry.info.clone()));
        }

        if self.forgotten.read().expect("forgotten").contains(p) {
            return Ok(None);
        }

        let Some(base) = &self.base else {
            return Ok(None);
        };
        let base = base.lock().expect("base db");
        Ok(base.query_path_info(&self.store_dir, p)?.map(|v| v.info))
    }

    pub fn is_valid(&self, p: &StorePath) -> anyhow::Result<bool> {
        Ok(self.info(p)?.is_some())
    }

    pub fn describe(
        nar: &[u8],
        references: BTreeSet<StorePath>,
        deriver: Option<StorePath>,
        ca: Option<ContentAddress>,
    ) -> UnkeyedValidPathInfo {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(1);
        UnkeyedValidPathInfo {
            deriver,
            nar_hash: NarHash::digest(nar),
            references,
            registration_time: NonZero::new(now),
            nar_size: nar.len() as u64,
            ultimate: false,
            signatures: Default::default(),
            ca,
            store_dir: StoreDir::default(),
        }
    }

    pub async fn register(
        &self,
        path: &StorePath,
        nar: &[u8],
        info: UnkeyedValidPathInfo,
        origin: Origin,
    ) -> anyhow::Result<Registered> {
        let _serial = self.registering.lock().await;
        if self.is_valid(path)? {
            return Ok(Registered::AlreadyValid);
        }

        if NarHash::digest(nar) != info.nar_hash {
            self.journal.violation(Violation::NarHashMismatch {
                path: path.to_string(),
            });
            anyhow::bail!("NAR hash mismatch for {path}");
        }

        for reference in info.references.iter().filter(|r| *r != path) {
            if !self.is_valid(reference)? {
                self.journal.violation(Violation::ReferenceNotValid {
                    path: path.to_string(),
                    reference: reference.to_string(),
                });
                anyhow::bail!("{path} references invalid {reference}");
            }
        }

        let dest = self.real_path(path);
        self.write_tree(&dest, nar).await?;
        self.forgotten.write().expect("forgotten").remove(path);
        self.overlay
            .write()
            .expect("overlay")
            .insert(path.clone(), Entry { info, origin });
        Ok(Registered::New)
    }

    async fn write_tree(&self, dest: &Path, nar: &[u8]) -> anyhow::Result<()> {
        remove_tree(dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let events = parse_nar(std::io::Cursor::new(nar.to_vec())).map(|event| {
            event.map_err(|e| NarWriteError::create_file_error(dest.to_path_buf(), e))
        });
        restore(events, dest).await?;
        canonicalize(dest)?;
        Ok(())
    }

    pub fn forget(&self, p: &StorePath) -> anyhow::Result<()> {
        let was_overlay = self.overlay.write().expect("overlay").remove(p).is_some();
        if was_overlay {
            remove_tree(&self.real_path(p))?;
        }

        self.forgotten.write().expect("forgotten").insert(p.clone());
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<(String, Origin)> {
        self.overlay
            .read()
            .expect("overlay")
            .iter()
            .map(|(p, e)| (p.to_string(), e.origin))
            .collect()
    }

    pub fn referrers(&self, p: &StorePath) -> StorePathSet {
        self.overlay
            .read()
            .expect("overlay")
            .iter()
            .filter(|(_, e)| e.info.references.contains(p))
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn path_from_hash_part(&self, hash: &str) -> anyhow::Result<Option<StorePath>> {
        let overlay = self.overlay.read().expect("overlay");
        Ok(overlay
            .keys()
            .find(|p| p.to_string().starts_with(hash))
            .cloned())
    }
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.is_dir() {
        return std::fs::remove_file(path);
    }

    for dir in directories(path) {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::remove_dir_all(path)
}

fn directories(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                out.extend(directories(&entry.path()));
            }
        }
    }
    out
}

fn canonicalize(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }

    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            canonicalize(&entry?.path())?;
        }
    }

    let exec = meta.is_dir() || meta.permissions().mode() & 0o100 != 0;
    let mode = if exec { 0o555 } else { 0o444 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    let epoch = UNIX_EPOCH + Duration::from_secs(1);
    let times = std::fs::FileTimes::new()
        .set_modified(epoch)
        .set_accessed(epoch);
    std::fs::File::open(path)?.set_times(times)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::nar::{NarFile, encode};

    fn store(dir: &tempfile::TempDir) -> (MockStore, Arc<Journal>) {
        let journal = Arc::new(Journal::new());
        let store = MockStore::open(dir.path().to_path_buf(), None, journal.clone()).expect("open");
        (store, journal)
    }

    fn nar(text: &str) -> bytes::Bytes {
        let file = NarFile {
            contents: text.as_bytes().to_vec(),
            executable: false,
        };
        encode(&BTreeMap::from([(String::new(), file)]))
    }

    fn sp(hash_char: char, name: &str) -> StorePath {
        StorePath::from_base_path(&format!("{}-{name}", hash_char.to_string().repeat(32)))
            .expect("path")
    }

    fn plain(bytes: &[u8]) -> UnkeyedValidPathInfo {
        MockStore::describe(bytes, Default::default(), None, None)
    }

    #[tokio::test]
    async fn register_writes_file_and_is_valid() {
        let dir = tempfile::tempdir().expect("tmp");
        let (s, journal) = store(&dir);
        let p = sp('0', "x");
        let bytes = nar("hello");
        let registered = s
            .register(&p, &bytes, plain(&bytes), Origin::Built)
            .await
            .expect("reg");
        assert_eq!(registered, Registered::New);
        assert!(s.is_valid(&p).expect("valid"));
        assert_eq!(std::fs::read(s.real_path(&p)).expect("file"), b"hello");
        assert!(journal.violations().is_empty());
    }

    #[tokio::test]
    async fn register_twice_is_a_noop() {
        let dir = tempfile::tempdir().expect("tmp");
        let (s, journal) = store(&dir);
        let p = sp('0', "x");
        let bytes = nar("hello");
        s.register(&p, &bytes, plain(&bytes), Origin::Evaluated)
            .await
            .expect("first");
        let second = s
            .register(&p, &bytes, plain(&bytes), Origin::Evaluated)
            .await
            .expect("second");
        assert_eq!(second, Registered::AlreadyValid);
        assert!(journal.violations().is_empty());
    }

    #[tokio::test]
    async fn missing_reference_is_a_violation_and_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let (s, journal) = store(&dir);
        let bytes = nar("hello");
        let info = MockStore::describe(&bytes, [sp('1', "dep")].into(), None, None);
        assert!(
            s.register(&sp('0', "x"), &bytes, info, Origin::Imported)
                .await
                .is_err()
        );
        assert!(matches!(
            journal.violations()[0],
            Violation::ReferenceNotValid { .. }
        ));
        assert!(!s.is_valid(&sp('0', "x")).expect("valid"));
    }

    #[tokio::test]
    async fn self_reference_is_allowed() {
        let dir = tempfile::tempdir().expect("tmp");
        let (s, journal) = store(&dir);
        let p = sp('0', "x");
        let bytes = nar("hello");
        let info = MockStore::describe(&bytes, [p.clone()].into(), None, None);
        s.register(&p, &bytes, info, Origin::Built)
            .await
            .expect("reg");
        assert!(journal.violations().is_empty());
    }

    #[tokio::test]
    async fn nar_hash_mismatch_is_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let (s, journal) = store(&dir);
        let info = plain(&nar("other"));
        assert!(
            s.register(&sp('0', "x"), &nar("hello"), info, Origin::Imported)
                .await
                .is_err()
        );
        assert!(matches!(
            journal.violations()[0],
            Violation::NarHashMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn forget_removes_overlay_path_from_disk() {
        let dir = tempfile::tempdir().expect("tmp");
        let (s, _) = store(&dir);
        let p = sp('0', "x");
        let bytes = nar("hello");
        s.register(&p, &bytes, plain(&bytes), Origin::Built)
            .await
            .expect("reg");
        s.forget(&p).expect("forget");
        assert!(!s.is_valid(&p).expect("valid"));
        assert!(!s.real_path(&p).exists());
        s.register(&p, &bytes, plain(&bytes), Origin::Imported)
            .await
            .expect("re-add");
    }

    #[tokio::test]
    async fn forget_base_path_only_masks() {
        let dir = tempfile::tempdir().expect("tmp");
        let journal = Arc::new(Journal::new());
        let db = dir.path().join("db.sqlite");
        let p = sp('9', "base");
        seed_base_db(&db, &p);
        std::fs::create_dir_all(dir.path().join("nix/store")).expect("mkdir");
        std::fs::write(dir.path().join("nix/store").join(p.to_string()), b"image").expect("write");
        let s = MockStore::open(dir.path().to_path_buf(), Some(&db), journal).expect("open");
        assert!(s.is_valid(&p).expect("valid"));
        s.forget(&p).expect("forget");
        assert!(!s.is_valid(&p).expect("masked"));
        assert!(s.real_path(&p).exists());
    }

    fn seed_base_db(db: &Path, p: &StorePath) {
        let base = StoreDb::open(db, OpenMode::Create).expect("db");
        base.create_schema().expect("schema");
        base.connection()
            .execute(
                "INSERT INTO ValidPaths (path, hash, registrationTime, narSize) VALUES (?1, ?2, 1, 5)",
                [format!("/nix/store/{p}"), format!("sha256:{}", "0".repeat(52))],
            )
            .expect("insert");
    }
}
