/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::future::Future;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use bytes::Bytes;
use gradient_wire::messages::ClientMessage;
use gradient_worker_client::connection::reconnecting::ReconnectingWriter;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex, watch};

pub const JOB_LOG_BUFFER_CAP: u64 = 1 << 30;

const REPLAY_CHUNK_BYTES: u64 = 256 * 1024;

pub trait LogChunkSender: Sync {
    fn send_now(&self, msg: ClientMessage) -> impl Future<Output = Result<()>> + Send;
}

impl LogChunkSender for ReconnectingWriter {
    fn send_now(&self, msg: ClientMessage) -> impl Future<Output = Result<()>> + Send {
        ReconnectingWriter::send_now(self, msg)
    }
}

#[derive(Clone)]
pub struct JobLog {
    inner: Arc<Inner>,
}

struct Inner {
    job_id: String,
    dir: PathBuf,
    log: PathBuf,
    offset: PathBuf,
    accepted_file: Option<PathBuf>,
    accepted: AtomicU64,
    cap: u64,
    state: Mutex<State>,
    drained: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    buffering: bool,
    file_len: u64,
    offset: u64,
    marker_len: u64,
    dropped: u64,
}

fn file_key(job_id: &str) -> String {
    job_id
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                char::from(b)
            } else {
                '_'
            }
        })
        .collect()
}

impl JobLog {
    pub fn open(dir: &Path, job_id: &str, cap: u64) -> Self {
        Self::with_state(dir, job_id, cap, false, State::default(), 0)
    }

    pub fn open_resumable(dir: &Path, job_id: &str, cap: u64) -> Self {
        Self::with_state(dir, job_id, cap, true, State::default(), 0)
    }

    fn with_state(
        dir: &Path,
        job_id: &str,
        cap: u64,
        resumable: bool,
        state: State,
        accepted: u64,
    ) -> Self {
        let key = file_key(job_id);
        let drained = !state.buffering;
        Self {
            inner: Arc::new(Inner {
                job_id: job_id.to_owned(),
                dir: dir.to_owned(),
                log: dir.join(format!("{key}.log")),
                offset: dir.join(format!("{key}.offset")),
                accepted_file: resumable.then(|| dir.join(format!("{key}.accepted"))),
                accepted: AtomicU64::new(accepted),
                cap,
                state: Mutex::new(state),
                drained: watch::Sender::new(drained),
            }),
        }
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a reattached exec job resumes its output from here"
        )
    )]
    pub fn accepted(&self) -> u64 {
        self.inner.accepted.load(Ordering::SeqCst)
    }

    pub async fn send(&self, writer: &impl LogChunkSender, data: Vec<u8>) -> Result<()> {
        self.deliver(writer, data, true).await
    }

    pub async fn send_note(&self, writer: &impl LogChunkSender, data: Vec<u8>) -> Result<()> {
        self.deliver(writer, data, false).await
    }

    pub async fn skip(&self, bytes: u64) -> Result<()> {
        let _state = self.inner.state.lock().await;
        self.count_accepted(bytes).await
    }

    async fn deliver(
        &self,
        writer: &impl LogChunkSender,
        data: Vec<u8>,
        counted: bool,
    ) -> Result<()> {
        let data = Bytes::from(data);
        let len = data.len() as u64;
        let mut state = self.inner.state.lock().await;
        let sent = !state.buffering && writer.send_now(self.chunk(data.clone())).await.is_ok();
        if !sent {
            if !state.buffering {
                state.buffering = true;
                self.inner.drained.send_replace(false);
            }
            self.append(&mut state, &data).await?;
        }
        if counted {
            self.count_accepted(len).await?;
        }
        Ok(())
    }

    async fn count_accepted(&self, bytes: u64) -> Result<()> {
        let accepted = self.inner.accepted.fetch_add(bytes, Ordering::SeqCst) + bytes;
        match &self.inner.accepted_file {
            Some(path) => store_number(path, accepted).await,
            None => Ok(()),
        }
    }

    pub async fn replay(&self, writer: &impl LogChunkSender) -> Result<()> {
        loop {
            let mut state = self.inner.state.lock().await;
            if !state.buffering {
                return Ok(());
            }
            if state.offset == state.file_len {
                self.remove();
                *state = State::default();
                self.inner.drained.send_replace(true);
                return Ok(());
            }
            let len = REPLAY_CHUNK_BYTES.min(state.file_len - state.offset);
            let chunk = self.read_at(state.offset, len).await?;
            writer.send_now(self.chunk(chunk.into())).await?;
            state.offset += len;
            self.store_offset(state.offset).await?;
        }
    }

    pub async fn drained(&self) {
        let mut drained = self.inner.drained.subscribe();
        let _ = drained.wait_for(|drained| *drained).await;
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.inner.log);
        let _ = std::fs::remove_file(&self.inner.offset);
        if let Some(accepted) = &self.inner.accepted_file {
            let _ = std::fs::remove_file(accepted);
        }
    }

    fn chunk(&self, data: Bytes) -> ClientMessage {
        ClientMessage::LogChunk {
            job_id: self.inner.job_id.clone(),
            data,
        }
    }

    async fn append(&self, state: &mut State, data: &[u8]) -> Result<()> {
        if state.file_len == 0 {
            tokio::fs::create_dir_all(&self.inner.dir)
                .await
                .with_context(|| format!("create job log dir {}", self.inner.dir.display()))?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.log)
            .await
            .with_context(|| format!("open job log {}", self.inner.log.display()))?;
        file.write_all(data).await.context("append to job log")?;
        file.flush().await.context("flush job log")?;
        state.file_len += data.len() as u64;
        let cap = self.inner.cap;
        if unsent_log_bytes(state) > cap + cap / 16 {
            self.drop_oldest(state).await?;
        }
        Ok(())
    }

    async fn drop_oldest(&self, state: &mut State) -> Result<()> {
        let cap = self.inner.cap;
        let carried = if state.offset < state.marker_len {
            state.dropped
        } else {
            0
        };
        let dropped = carried + unsent_log_bytes(state) - cap;
        let marker = format!(
            "\n[gradient] {dropped} bytes of log dropped while the server was unreachable\n"
        );

        let rewritten = self.inner.log.with_extension("log.tmp");
        let mut target = tokio::fs::File::create(&rewritten)
            .await
            .context("create rewritten job log")?;
        target.write_all(marker.as_bytes()).await?;
        let mut source = tokio::fs::File::open(&self.inner.log).await?;
        source.seek(SeekFrom::Start(state.file_len - cap)).await?;
        tokio::io::copy(&mut source.take(cap), &mut target).await?;
        target.flush().await?;
        tokio::fs::rename(&rewritten, &self.inner.log)
            .await
            .context("replace job log")?;

        state.marker_len = marker.len() as u64;
        state.dropped = dropped;
        state.file_len = state.marker_len + cap;
        state.offset = 0;
        self.store_offset(0).await
    }

    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let mut file = tokio::fs::File::open(&self.inner.log)
            .await
            .with_context(|| format!("open job log {}", self.inner.log.display()))?;
        file.seek(SeekFrom::Start(offset)).await?;
        let mut chunk = vec![0; usize::try_from(len).context("replay chunk size")?];
        file.read_exact(&mut chunk).await.context("read job log")?;
        Ok(chunk)
    }

    async fn store_offset(&self, offset: u64) -> Result<()> {
        store_number(&self.inner.offset, offset).await
    }
}

async fn store_number(path: &Path, value: u64) -> Result<()> {
    let mut written = path.as_os_str().to_owned();
    written.push(".tmp");
    tokio::fs::write(&written, value.to_string()).await?;
    tokio::fs::rename(&written, path)
        .await
        .with_context(|| format!("store {}", path.display()))
}

fn unsent_log_bytes(state: &State) -> u64 {
    state.file_len - state.offset.max(state.marker_len)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
    )]

    use super::*;
    use gradient_util::sync::Mutex as SyncMutex;
    use gradient_worker_client::connection::reconnecting::SessionLost;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;

    struct Recorder {
        accepts: AtomicUsize,
        received: SyncMutex<Vec<u8>>,
        chunks: AtomicUsize,
        permits: Option<Semaphore>,
    }

    impl Recorder {
        fn accepting(accepts: usize) -> Self {
            Self {
                accepts: AtomicUsize::new(accepts),
                received: SyncMutex::new(Vec::new()),
                chunks: AtomicUsize::new(0),
                permits: None,
            }
        }

        fn detached() -> Self {
            Self::accepting(0)
        }

        fn attached() -> Self {
            Self::accepting(usize::MAX)
        }

        fn gated() -> Self {
            Self {
                permits: Some(Semaphore::new(0)),
                ..Self::attached()
            }
        }

        fn received(&self) -> Vec<u8> {
            self.received.lock().clone()
        }
    }

    impl LogChunkSender for Recorder {
        async fn send_now(&self, msg: ClientMessage) -> Result<()> {
            if let Some(permits) = &self.permits {
                permits.acquire().await?.forget();
            }
            let ClientMessage::LogChunk { data, .. } = msg else {
                panic!("only log chunks")
            };
            if self
                .accepts
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err()
            {
                return Err(SessionLost.into());
            }
            self.received.lock().extend_from_slice(&data);
            self.chunks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn chunk_len() -> usize {
        usize::try_from(REPLAY_CHUNK_BYTES).unwrap()
    }

    #[tokio::test]
    async fn chunks_sent_while_detached_replay_in_order_after_attach() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open(dir.path(), "build:x", JOB_LOG_BUFFER_CAP);
        let detached = Recorder::detached();
        log.send(&detached, b"a".to_vec()).await.unwrap();
        log.send(&detached, b"b".to_vec()).await.unwrap();
        assert!(detached.received().is_empty());

        let attached = Recorder::attached();
        log.replay(&attached).await.unwrap();

        assert_eq!(attached.received(), b"ab");
        assert!(!dir.path().join("build_x.log").exists());
        assert!(!dir.path().join("build_x.offset").exists());
    }

    #[tokio::test]
    async fn notes_leave_the_accepted_count_and_skipped_bytes_join_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open_resumable(dir.path(), "exec:x", JOB_LOG_BUFFER_CAP);
        let attached = Recorder::attached();

        log.send(&attached, b"hello".to_vec()).await.unwrap();
        log.send_note(&attached, b"[note]".to_vec()).await.unwrap();
        log.skip(3).await.unwrap();

        assert_eq!(attached.received(), b"hello[note]");
        assert_eq!(log.accepted(), 8);
        let stored = std::fs::read_to_string(dir.path().join("exec_x.accepted")).unwrap();
        assert_eq!(stored, "8");
    }

    #[tokio::test]
    async fn a_full_buffer_drops_the_oldest_bytes_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open(dir.path(), "build:x", 8);
        let detached = Recorder::detached();
        for chunk in b"0123456789abcdef".chunks(4) {
            log.send(&detached, chunk.to_vec()).await.unwrap();
        }

        let attached = Recorder::attached();
        log.replay(&attached).await.unwrap();

        let received = String::from_utf8(attached.received()).unwrap();
        let (marker, kept) = received.trim_start_matches('\n').split_once('\n').unwrap();
        assert!(marker.contains("bytes of log dropped"), "{marker}");
        assert_eq!(kept, "89abcdef");
    }

    #[tokio::test]
    async fn lines_written_during_a_replay_follow_the_replayed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open(dir.path(), "build:x", JOB_LOG_BUFFER_CAP);
        let buffered = vec![7u8; 4 * chunk_len()];
        log.send(&Recorder::detached(), buffered.clone())
            .await
            .unwrap();

        let attached = Arc::new(Recorder::gated());
        let replay = tokio::spawn({
            let (log, attached) = (log.clone(), Arc::clone(&attached));
            async move { log.replay(&*attached).await }
        });
        attached.permits.as_ref().unwrap().add_permits(1);
        while attached.chunks.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let live = tokio::spawn({
            let (log, attached) = (log.clone(), Arc::clone(&attached));
            async move { log.send(&*attached, b"live".to_vec()).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        attached.permits.as_ref().unwrap().add_permits(100);
        replay.await.unwrap().unwrap();
        live.await.unwrap().unwrap();

        let received = attached.received();
        assert_eq!(&received[..buffered.len()], &buffered[..]);
        assert_eq!(&received[buffered.len()..], b"live");
    }

    #[tokio::test]
    async fn a_replay_cut_by_a_session_loss_resumes_at_the_stored_offset() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open(dir.path(), "build:x", JOB_LOG_BUFFER_CAP);
        let detached = Recorder::detached();
        let chunks: Vec<Vec<u8>> = (0u8..3).map(|i| vec![i; chunk_len()]).collect();
        for chunk in &chunks {
            log.send(&detached, chunk.clone()).await.unwrap();
        }

        let cut = Recorder::accepting(1);
        assert!(log.replay(&cut).await.is_err());
        assert_eq!(cut.received(), chunks[0]);
        let stored = std::fs::read_to_string(dir.path().join("build_x.offset")).unwrap();
        assert_eq!(stored, chunk_len().to_string());

        let next = Recorder::attached();
        log.replay(&next).await.unwrap();
        assert_eq!(next.received(), [&chunks[1][..], &chunks[2][..]].concat());
    }

    #[tokio::test]
    async fn drained_waits_until_the_buffer_is_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let log = JobLog::open(dir.path(), "build:x", JOB_LOG_BUFFER_CAP);
        log.send(&Recorder::detached(), b"pending".to_vec())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), log.drained())
                .await
                .is_err()
        );

        log.replay(&Recorder::attached()).await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), log.drained())
            .await
            .expect("a replayed buffer is drained");
    }
}
