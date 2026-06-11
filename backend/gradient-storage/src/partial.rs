/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! On-disk store for in-progress (`*.partial`) NAR transfers.
//!
//! A receiver persists incoming compressed chunks to `{root}/{key}.partial`
//! with a sibling `{root}/{key}.token` sidecar recording the sender's
//! `stream_token`. On resume the receiver reports the current partial length;
//! the sender seeks to it. A `stream_token` mismatch (e.g. a worker upgrade
//! changed zstd output) truncates the partial so the transfer restarts from 0.
//!
//! Keys must be filesystem-safe. Callers use the NAR hash (worker pull) or
//! `{peer_id}/{hash}` (server push, namespaced so two workers pushing the same
//! content never share a file). Appends enforce contiguous offsets.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct PartialStore {
    root: PathBuf,
    ttl: Duration,
}

impl PartialStore {
    /// Create the store rooted at `root`, creating the directory if needed.
    pub fn new(root: impl Into<PathBuf>, ttl: Duration) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)
            .with_context(|| format!("create partial dir {}", root.display()))?;
        Ok(Self { root, ttl })
    }

    fn partial_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.partial"))
    }

    fn token_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.token"))
    }

    /// Path to the `.partial` file so callers can stream/read it directly.
    pub fn path(&self, key: &str) -> PathBuf {
        self.partial_path(key)
    }

    /// Ensure any parent directory implied by a `{peer}/{hash}` key exists.
    fn ensure_parent(&self, key: &str) -> Result<()> {
        if let Some(parent) = self.partial_path(key).parent()
            && parent != self.root.as_path()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create partial parent {}", parent.display()))?;
        }

        Ok(())
    }

    /// Bytes already received for `key` under `token`. Returns 0 (and discards
    /// any existing partial) when the stored token differs from `token`.
    pub fn received_len(&self, key: &str, token: &str) -> Result<u64> {
        let stored = match fs::read_to_string(self.token_path(key)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).context("read partial token"),
        };

        if stored != token {
            self.discard(key)?;
            return Ok(0);
        }

        match fs::metadata(self.partial_path(key)) {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).context("stat partial"),
        }
    }

    /// Append `data` at `offset`. `offset` must equal the current partial
    /// length (contiguous); a gap or overlap is an error. `offset == 0` always
    /// truncates any stale prefix and starts fresh under `token` - so a sender
    /// that restarts a transfer from the beginning (e.g. a reconnect without a
    /// resume handshake) never trips the contiguity check.
    pub fn append(&self, key: &str, token: &str, offset: u64, data: &[u8]) -> Result<()> {
        self.ensure_parent(key)?;
        let path = self.partial_path(key);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open partial {}", path.display()))?;

        if offset == 0 {
            file.set_len(0).context("truncate partial for fresh start")?;
        } else {
            let len = file.metadata().context("stat partial for append")?.len();
            if offset != len {
                bail!("non-contiguous partial append for {key}: offset {offset} != len {len}");
            }
        }

        fs::write(self.token_path(key), token).context("write partial token")?;
        file.seek(SeekFrom::End(0)).context("seek partial end")?;
        file.write_all(data).context("write partial chunk")?;
        file.flush().context("flush partial")?;
        Ok(())
    }

    /// Current length of the partial regardless of token (0 if absent). Use
    /// with [`Self::token`] when resuming a transfer whose token the caller
    /// learned on a prior attempt.
    pub fn staged_len(&self, key: &str) -> u64 {
        fs::metadata(self.partial_path(key))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// The `stream_token` stored alongside the partial, if any.
    pub fn token(&self, key: &str) -> Option<String> {
        fs::read_to_string(self.token_path(key)).ok()
    }

    /// Read the whole partial into memory (small/medium NARs only — prefer
    /// streaming [`Self::path`] for large commits).
    pub fn read_all(&self, key: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        fs::File::open(self.partial_path(key))
            .with_context(|| format!("open partial {key} for read"))?
            .read_to_end(&mut buf)
            .context("read partial")?;
        Ok(buf)
    }

    /// Remove the partial and its token sidecar (idempotent).
    pub fn discard(&self, key: &str) -> Result<()> {
        for p in [self.partial_path(key), self.token_path(key)] {
            match fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("remove {}", p.display())),
            }
        }

        Ok(())
    }

    /// Total bytes across all `.partial` files (disk-budget accounting).
    pub fn total_bytes(&self) -> Result<u64> {
        Ok(self.walk()?.iter().map(|(_, len, _)| *len).sum())
    }

    /// Delete partials whose mtime is older than the TTL. Returns the count
    /// removed. A zero TTL disables the sweep.
    pub fn gc(&self) -> Result<usize> {
        if self.ttl.is_zero() {
            return Ok(0);
        }

        let cutoff = SystemTime::now()
            .checked_sub(self.ttl)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = 0;
        for (path, _len, mtime) in self.walk()? {
            if mtime >= cutoff {
                continue;
            }

            let key = path
                .strip_prefix(&self.root)
                .ok()
                .and_then(|p| p.to_str())
                .and_then(|s| s.strip_suffix(".partial"))
                .map(str::to_owned);
            if let Some(key) = key {
                self.discard(&key)?;
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Recursive walk returning `(partial_path, len, mtime)` for every
    /// `*.partial` file under the root (one level of `{peer}/` nesting).
    fn walk(&self) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
        let mut out = Vec::new();
        self.walk_dir(&self.root, &mut out)?;
        Ok(out)
    }

    fn walk_dir(&self, dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).context("read partial dir"),
        };

        for entry in entries {
            let entry = entry.context("partial dir entry")?;
            let path = entry.path();
            let meta = entry.metadata().context("partial entry metadata")?;
            if meta.is_dir() {
                self.walk_dir(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("partial") {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((path, meta.len(), mtime));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(ttl_secs: u64) -> (TempDir, PartialStore) {
        let dir = TempDir::new().unwrap();
        let s = PartialStore::new(dir.path(), Duration::from_secs(ttl_secs)).unwrap();
        (dir, s)
    }

    #[test]
    fn append_then_resume_reports_len() {
        let (_d, s) = store(3600);
        s.append("abc", "tok", 0, b"hello").unwrap();
        s.append("abc", "tok", 5, b" world").unwrap();
        assert_eq!(s.received_len("abc", "tok").unwrap(), 11);
        assert_eq!(s.read_all("abc").unwrap(), b"hello world");
    }

    #[test]
    fn non_contiguous_append_errors() {
        let (_d, s) = store(3600);
        s.append("abc", "tok", 0, b"hello").unwrap();
        assert!(s.append("abc", "tok", 7, b"world").is_err());
    }

    #[test]
    fn token_mismatch_truncates_to_zero() {
        let (_d, s) = store(3600);
        s.append("abc", "old", 0, b"hello").unwrap();
        assert_eq!(s.received_len("abc", "new").unwrap(), 0);
        s.append("abc", "new", 0, b"x").unwrap();
        assert_eq!(s.read_all("abc").unwrap(), b"x");
    }

    #[test]
    fn discard_is_idempotent() {
        let (_d, s) = store(3600);
        s.append("abc", "tok", 0, b"hello").unwrap();
        s.discard("abc").unwrap();
        s.discard("abc").unwrap();
        assert_eq!(s.received_len("abc", "tok").unwrap(), 0);
    }

    #[test]
    fn namespaced_key_creates_subdir() {
        let (_d, s) = store(3600);
        s.append("peer-1/abc", "tok", 0, b"hi").unwrap();
        assert_eq!(s.received_len("peer-1/abc", "tok").unwrap(), 2);
    }

    #[test]
    fn total_bytes_sums_partials() {
        let (_d, s) = store(3600);
        s.append("a", "t", 0, b"12345").unwrap();
        s.append("peer/b", "t", 0, b"678").unwrap();
        assert_eq!(s.total_bytes().unwrap(), 8);
    }

    #[test]
    fn gc_zero_ttl_disabled() {
        let (_d, s) = store(0);
        s.append("a", "t", 0, b"123").unwrap();
        assert_eq!(s.gc().unwrap(), 0);
        assert_eq!(s.received_len("a", "t").unwrap(), 3);
    }
}
