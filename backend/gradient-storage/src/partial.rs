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
//! Keys must be filesystem-safe. Callers use `{job_id}/{hash}` (worker pull) or
//! `{peer_id}/{job_id}/{hash}` (server push), namespaced by job so two concurrent
//! transfers of the same content-addressed path never share a file. Appends
//! enforce contiguous offsets.
//!
//! All filesystem access is async (`tokio::fs`) so staging never parks a tokio
//! worker thread on the hot NAR receive path.

use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use harmonia_utils_hash::{Algorithm, Context as HashContext, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Process-local sequence giving each [`PartialStore::detach`] claim a unique
/// key, so concurrent claims of the same content-addressed hash never collide.
static CLAIM_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct PartialStore {
    root: PathBuf,
    ttl: Duration,
}

/// A completed `.partial` file: where it lies, how many bytes it holds and the
/// SHA-256 the writer computed while staging them, so a commit verifies the
/// transfer without re-reading the file.
#[derive(Clone, Debug)]
pub struct StagedFile {
    pub path: PathBuf,
    pub len: u64,
    pub sha256: [u8; 32],
}

/// An open append handle on one `.partial`, owned by the task draining a single
/// transfer. Holding the file open across a stream turns the per-chunk
/// open/stat/seek/token-rewrite into one syscall per chunk, and hashing as the
/// bytes go by removes the commit's verification read entirely.
pub struct PartialWriter {
    file: tokio::fs::File,
    path: PathBuf,
    len: u64,
    hasher: HashContext,
    /// Bytes of a resumed prefix still to fold into `hasher`, read on first use
    /// so a resume's full-file read lands on whoever writes the stream rather
    /// than on the caller that opened it.
    pending_prefix: u64,
}

impl PartialWriter {
    /// Bytes staged so far, i.e. the offset the next chunk must carry.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append `data` at `offset`, which must equal [`Self::len`].
    pub async fn append(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        if offset != self.len {
            bail!(
                "non-contiguous partial append: offset {offset} != len {}",
                self.len
            );
        }

        self.hash_prefix().await?;
        self.file
            .write_all(data)
            .await
            .context("write partial chunk")?;
        self.hasher.update(data);
        self.len += data.len() as u64;
        Ok(())
    }

    /// Flush and close, reporting what was staged.
    pub async fn finish(mut self) -> Result<StagedFile> {
        self.hash_prefix().await?;
        self.file.flush().await.context("flush partial")?;
        let digest = Sha256::try_from(self.hasher.finish())
            .map_err(|_| anyhow::anyhow!("sha256 finalize produced a non-sha256 digest"))?;
        Ok(StagedFile {
            path: self.path,
            len: self.len,
            sha256: *digest.digest_bytes(),
        })
    }

    /// Fold a resumed prefix into the hash. Runs at most once, before the first
    /// byte is written, so the file holds exactly the prefix and reading it to
    /// end reads all of it.
    async fn hash_prefix(&mut self) -> Result<()> {
        if self.pending_prefix == 0 {
            return Ok(());
        }

        self.file
            .seek(SeekFrom::Start(0))
            .await
            .context("seek partial start")?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = self
                .file
                .read(&mut buf)
                .await
                .context("rehash partial prefix")?;
            if n == 0 {
                break;
            }
            self.hasher.update(&buf[..n]);
        }

        self.file
            .seek(SeekFrom::End(0))
            .await
            .context("seek partial end")?;
        self.pending_prefix = 0;
        Ok(())
    }
}

impl PartialStore {
    /// Create the store rooted at `root`, creating the directory if needed. The
    /// one-time sync `create_dir_all` keeps `new` non-async so constructors need
    /// not cascade into an async context.
    pub fn new(root: impl Into<PathBuf>, ttl: Duration) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
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

    /// Open the staged `.partial` for streaming reads, so a large NAR can be
    /// verified and uploaded without `read_all` buffering it whole in memory.
    pub async fn open_read(&self, key: &str) -> Result<tokio::fs::File> {
        let path = self.partial_path(key);
        tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("open staged partial {}", path.display()))
    }

    /// Ensure any parent directory implied by a `{peer}/{hash}` key exists.
    async fn ensure_parent(&self, key: &str) -> Result<()> {
        if let Some(parent) = self.partial_path(key).parent()
            && parent != self.root.as_path()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create partial parent {}", parent.display()))?;
        }

        Ok(())
    }

    /// Bytes already received for `key` under `token`. Returns 0 (and discards
    /// any existing partial) when the stored token differs from `token`.
    pub async fn received_len(&self, key: &str, token: &str) -> Result<u64> {
        let stored = match tokio::fs::read_to_string(self.token_path(key)).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).context("read partial token"),
        };

        if stored != token {
            self.discard(key).await?;
            return Ok(0);
        }

        match tokio::fs::metadata(self.partial_path(key)).await {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).context("stat partial"),
        }
    }

    /// Open `key` for appending under `token`. `resume_from > 0` keeps the
    /// stored prefix (already validated by [`Self::received_len`]) and hashes it
    /// on the writer's first use; `0` truncates and records `token` as the
    /// sidecar. Opening costs a handful of syscalls, so it is safe on a hot loop.
    pub async fn open_writer(
        &self,
        key: &str,
        token: &str,
        resume_from: u64,
    ) -> Result<PartialWriter> {
        self.ensure_parent(key).await?;
        let path = self.partial_path(key);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .await
            .with_context(|| format!("open partial {}", path.display()))?;

        if resume_from == 0 {
            file.set_len(0)
                .await
                .context("truncate partial for fresh start")?;
            tokio::fs::write(self.token_path(key), token)
                .await
                .context("write partial token")?;
        } else {
            let len = file.metadata().await.context("stat partial")?.len();
            if len != resume_from {
                bail!("partial {key} holds {len} bytes, resume asked for {resume_from}");
            }
        }

        file.seek(SeekFrom::End(0))
            .await
            .context("seek partial end")?;
        Ok(PartialWriter {
            file,
            path,
            len: resume_from,
            hasher: HashContext::new(Algorithm::SHA256),
            pending_prefix: resume_from,
        })
    }

    /// Append `data` at `offset`. `offset` must equal the current partial
    /// length (contiguous); a gap or overlap is an error. `offset == 0` always
    /// truncates any stale prefix and starts fresh under `token` - so an HTTP
    /// uploader that restarts a transfer from the beginning never trips the
    /// contiguity check. That tolerance is this call's alone: a `/proto` push
    /// stream goes through [`Self::open_writer`] and is append-only once open,
    /// so it restarts by re-opening the writer, not by re-sending offset 0.
    pub async fn append(&self, key: &str, token: &str, offset: u64, data: &[u8]) -> Result<()> {
        self.ensure_parent(key).await?;
        let path = self.partial_path(key);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .await
            .with_context(|| format!("open partial {}", path.display()))?;

        if offset == 0 {
            file.set_len(0)
                .await
                .context("truncate partial for fresh start")?;
            tokio::fs::write(self.token_path(key), token)
                .await
                .context("write partial token")?;
        } else {
            let len = file
                .metadata()
                .await
                .context("stat partial for append")?
                .len();
            if offset != len {
                bail!("non-contiguous partial append for {key}: offset {offset} != len {len}");
            }
        }

        file.seek(SeekFrom::End(0))
            .await
            .context("seek partial end")?;
        file.write_all(data).await.context("write partial chunk")?;
        file.flush().await.context("flush partial")?;
        Ok(())
    }

    /// Current length of the partial regardless of token (0 if absent). Use
    /// with [`Self::token`] when resuming a transfer whose token the caller
    /// learned on a prior attempt.
    pub async fn staged_len(&self, key: &str) -> u64 {
        tokio::fs::metadata(self.partial_path(key))
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// The `stream_token` stored alongside the partial, if any.
    pub async fn token(&self, key: &str) -> Option<String> {
        tokio::fs::read_to_string(self.token_path(key)).await.ok()
    }

    /// Read the whole partial into memory (small/medium NARs only - prefer
    /// streaming [`Self::path`] for large commits).
    pub async fn read_all(&self, key: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        tokio::fs::File::open(self.partial_path(key))
            .await
            .with_context(|| format!("open partial {key} for read"))?
            .read_to_end(&mut buf)
            .await
            .context("read partial")?;
        Ok(buf)
    }

    /// Atomically claim the completed partial for `key` under a fresh,
    /// process-unique key, returning that claim key. A detached commit reads the
    /// claim while a later push of the same content-addressed hash resets the
    /// shared `{key}` partial - a token mismatch on the next header discards it
    /// ([`Self::received_len`]) and an `offset == 0` open truncates it - so
    /// without the claim the queued commit would read the wrong bytes. Returns
    /// `None` when nothing is staged. The rename is O(1) and safe on the read
    /// loop; only the byte copy/upload stays detached.
    ///
    /// The token sidecar is dropped rather than moved: a claim is terminal, so
    /// nothing can resume it, and only a `.partial` is reachable by [`Self::gc`].
    pub async fn detach(&self, key: &str) -> Result<Option<String>> {
        let src = self.partial_path(key);
        match tokio::fs::metadata(&src).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("stat partial to claim"),
        }

        let seq = CLAIM_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let claim = format!("{key}.{seq}.claim");
        self.ensure_parent(&claim).await?;
        tokio::fs::rename(&src, self.partial_path(&claim))
            .await
            .context("claim partial")?;
        match tokio::fs::remove_file(self.token_path(key)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("drop claimed partial token"),
        }
        Ok(Some(claim))
    }

    /// Remove the token sidecar, leaving the partial in place (idempotent).
    /// Used when a partial is committed under its own name: the sweep in
    /// [`Self::gc`] enumerates `*.partial` only, so a token left beside a file
    /// that is about to be renamed away would never be reclaimed.
    pub async fn discard_token(&self, key: &str) -> Result<()> {
        let path = self.token_path(key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }

    /// Remove the partial and its token sidecar (idempotent).
    pub async fn discard(&self, key: &str) -> Result<()> {
        for p in [self.partial_path(key), self.token_path(key)] {
            match tokio::fs::remove_file(&p).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("remove {}", p.display())),
            }
        }

        Ok(())
    }

    /// Total bytes across all `.partial` files (disk-budget accounting).
    pub async fn total_bytes(&self) -> Result<u64> {
        Ok(self.walk().await?.iter().map(|(_, len, _)| *len).sum())
    }

    /// Delete partials whose mtime is older than the TTL. Returns the count
    /// removed. A zero TTL disables the sweep.
    pub async fn gc(&self) -> Result<usize> {
        if self.ttl.is_zero() {
            return Ok(0);
        }

        let cutoff = SystemTime::now()
            .checked_sub(self.ttl)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = 0;
        for (path, _len, mtime) in self.walk().await? {
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
                self.discard(&key).await?;
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Walk returning `(partial_path, len, mtime)` for every `*.partial` file
    /// under the root. Uses an explicit dir stack (subdirs pushed, popped and
    /// read in turn) instead of async recursion, so one level of `{peer}/`
    /// nesting is handled without boxed futures.
    async fn walk(&self) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).context("read partial dir"),
            };

            while let Some(entry) = rd.next_entry().await.context("partial dir entry")? {
                let path = entry.path();
                let meta = entry.metadata().await.context("partial entry metadata")?;
                if meta.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("partial") {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    out.push((path, meta.len(), mtime));
                }
            }
        }

        Ok(out)
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

    #[tokio::test]
    async fn append_then_resume_reports_len() {
        let (_d, s) = store(3600);
        s.append("abc", "tok", 0, b"hello").await.unwrap();
        s.append("abc", "tok", 5, b" world").await.unwrap();
        assert_eq!(s.received_len("abc", "tok").await.unwrap(), 11);
        assert_eq!(s.read_all("abc").await.unwrap(), b"hello world");
    }

    #[tokio::test]
    async fn non_contiguous_append_errors() {
        let (_d, s) = store(3600);
        s.append("abc", "tok", 0, b"hello").await.unwrap();
        assert!(s.append("abc", "tok", 7, b"world").await.is_err());
    }

    #[tokio::test]
    async fn token_mismatch_truncates_to_zero() {
        let (_d, s) = store(3600);
        s.append("abc", "old", 0, b"hello").await.unwrap();
        assert_eq!(s.received_len("abc", "new").await.unwrap(), 0);
        s.append("abc", "new", 0, b"x").await.unwrap();
        assert_eq!(s.read_all("abc").await.unwrap(), b"x");
    }

    #[tokio::test]
    async fn discard_is_idempotent() {
        let (_d, s) = store(3600);
        s.append("abc", "tok", 0, b"hello").await.unwrap();
        s.discard("abc").await.unwrap();
        s.discard("abc").await.unwrap();
        assert_eq!(s.received_len("abc", "tok").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn namespaced_key_creates_subdir() {
        let (_d, s) = store(3600);
        s.append("peer-1/abc", "tok", 0, b"hi").await.unwrap();
        assert_eq!(s.received_len("peer-1/abc", "tok").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn total_bytes_sums_partials() {
        let (_d, s) = store(3600);
        s.append("a", "t", 0, b"12345").await.unwrap();
        s.append("peer/b", "t", 0, b"678").await.unwrap();
        assert_eq!(s.total_bytes().await.unwrap(), 8);
    }

    #[tokio::test]
    async fn gc_zero_ttl_disabled() {
        let (_d, s) = store(0);
        s.append("a", "t", 0, b"123").await.unwrap();
        assert_eq!(s.gc().await.unwrap(), 0);
        assert_eq!(s.received_len("a", "t").await.unwrap(), 3);
    }

    /// A claimed partial survives a later push that resets the shared key: the
    /// claim keeps the original token+bytes while the re-push starts fresh on
    /// the shared key. Regression: the detached commit read 0 bytes when a
    /// same-hash re-push discarded/truncated the shared `{peer}/{hash}` partial.
    #[tokio::test]
    async fn detach_isolates_claim_from_reset() {
        let (_d, s) = store(3600);
        s.append("peer/abc", "tok1", 0, b"hello").await.unwrap();

        let claim = s
            .detach("peer/abc")
            .await
            .unwrap()
            .expect("something staged");
        assert_ne!(claim, "peer/abc");

        // A later same-hash push resets the shared key (the claim took its token).
        assert_eq!(s.received_len("peer/abc", "tok2").await.unwrap(), 0);
        s.append("peer/abc", "tok2", 0, b"world!!").await.unwrap();

        // The claim is untouched: its bytes are intact and resume-proof.
        assert_eq!(s.staged_len(&claim).await, 5);
        assert!(s.token(&claim).await.is_none());
        assert_eq!(s.read_all(&claim).await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn writer_hashes_what_it_writes_and_finish_reports_it() {
        let (_d, s) = store(3600);
        let mut w = s.open_writer("peer/job/hash", "tok", 0).await.unwrap();
        w.append(0, b"hello ").await.unwrap();
        w.append(6, b"world").await.unwrap();
        let staged = w.finish().await.unwrap();
        assert_eq!(staged.len, 11);
        assert_eq!(
            &staged.sha256,
            Sha256::digest(b"hello world").digest_bytes()
        );
        assert_eq!(tokio::fs::read(&staged.path).await.unwrap(), b"hello world");
    }

    #[tokio::test]
    async fn resuming_rehashes_the_existing_prefix() {
        let (_d, s) = store(3600);
        let mut w = s.open_writer("k", "tok", 0).await.unwrap();
        w.append(0, b"hello ").await.unwrap();
        drop(w);
        let received = s.received_len("k", "tok").await.unwrap();
        assert_eq!(received, 6);
        let mut w = s.open_writer("k", "tok", received).await.unwrap();
        w.append(6, b"world").await.unwrap();
        let staged = w.finish().await.unwrap();
        assert_eq!(
            &staged.sha256,
            Sha256::digest(b"hello world").digest_bytes()
        );
    }

    /// A reconnect that has nothing left to send still has to report the hash
    /// of what is already on disk, so the deferred prefix rehash must run even
    /// when no chunk is ever appended.
    #[tokio::test]
    async fn a_resume_with_no_new_bytes_still_reports_the_prefix_hash() {
        let (_d, s) = store(3600);
        let mut w = s.open_writer("k", "tok", 0).await.unwrap();
        w.append(0, b"hello world").await.unwrap();
        w.finish().await.unwrap();

        let received = s.received_len("k", "tok").await.unwrap();
        let staged = s
            .open_writer("k", "tok", received)
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
        assert_eq!(staged.len, 11);
        assert_eq!(
            &staged.sha256,
            Sha256::digest(b"hello world").digest_bytes()
        );
    }

    #[tokio::test]
    async fn a_gap_is_rejected() {
        let (_d, s) = store(3600);
        let mut w = s.open_writer("k", "tok", 0).await.unwrap();
        w.append(0, b"abc").await.unwrap();
        assert!(w.append(5, b"x").await.is_err());
    }

    #[tokio::test]
    async fn detach_absent_is_none() {
        let (_d, s) = store(3600);
        assert!(s.detach("peer/missing").await.unwrap().is_none());
    }
}
