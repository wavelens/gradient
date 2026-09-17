/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! NAR transfer: inbound push staging, outbound serving, and the
//! `DispatchContext` handlers that commit an upload once it is complete.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use futures::StreamExt;
use gradient_core::ServerState;
use gradient_graph::Demotion;
use gradient_scheduler::Scheduler;
use gradient_storage::{NarSource, PartialWriter, StagedFile};
use gradient_util::shutdown::Shutdown;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, trace, warn};

use crate::ingest::WriteNeeded;
use crate::messages::{ArchivedClientMessage, ClientMessage, ServerMessage};
use crate::session::frame::Frame;

use super::dispatch::DispatchContext;
use super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::socket::{BULK_CHUNK_SIZE, ProtoWriter, send_server_msg};

// ── In-flight commits, per job ────────────────────────────────────────────────

/// The commits a job still owes the index.
///
/// Committing a NAR reads the whole staged file and writes it to `nar_storage`
/// (an S3 upload on object-store backends), so it runs detached: inline it froze
/// the session read loop and every concurrent transfer on the connection stalled.
/// That detachment is what lets `JobCompleted` - which rides the control writer
/// lane, drained first - overtake its own job's commits. The anchor then reaches
/// terminal success while the index has not yet been told the bytes exist, and a
/// commit that fails afterwards cannot correct it: [`BuildStateMachine`] refuses
/// to leave a terminal status, so the failure is dropped and the graph is left
/// trusting an output nothing serves. That is the state the unbacked-output heal
/// spends rebuilds on (#654).
///
/// So a job's completion waits here for its own commits. `settle` reports whether
/// they all landed; when one did not, the commit path has already failed the build
/// and the completion must not overwrite that verdict.
///
/// [`BuildStateMachine`]: gradient_db::state_machine::BuildStateMachine
#[derive(Default)]
pub(super) struct CommitTracker {
    state: gradient_util::sync::Mutex<TrackerState>,
    idle: tokio::sync::Notify,
}

#[derive(Default)]
struct TrackerState {
    in_flight: HashMap<String, usize>,
    failed: HashSet<String>,
}

impl CommitTracker {
    /// Register one commit for `job_id`. The guard releases it on every exit of
    /// the spawned task, an early return and a panic included.
    fn start(self: &Arc<Self>, job_id: &str) -> CommitGuard {
        *self
            .state
            .lock()
            .in_flight
            .entry(job_id.to_owned())
            .or_insert(0) += 1;

        CommitGuard {
            tracker: Arc::clone(self),
            job_id: job_id.to_owned(),
        }
    }

    /// Wait until `job_id` owes no commit, and report whether they all landed.
    /// Forgets the job, so a second call after a completion reads a clean slate.
    pub(super) async fn settle(&self, job_id: &str) -> bool {
        loop {
            // Registered before the count is read: a guard that drops in between
            // would otherwise notify nobody and this would wait for a commit that
            // has already finished.
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();

            {
                let mut state = self.state.lock();
                if state.in_flight.get(job_id).copied().unwrap_or(0) == 0 {
                    state.in_flight.remove(job_id);
                    return !state.failed.remove(job_id);
                }
            }

            idle.await;
        }
    }
}

/// One in-flight commit. Dropping it releases the job's claim; `fail` records
/// that this commit did not land before it is dropped.
pub(super) struct CommitGuard {
    tracker: Arc<CommitTracker>,
    job_id: String,
}

impl CommitGuard {
    fn fail(&self) {
        self.tracker.state.lock().failed.insert(self.job_id.clone());
    }
}

impl Drop for CommitGuard {
    fn drop(&mut self) {
        let mut state = self.tracker.state.lock();
        if let Some(n) = state.in_flight.get_mut(&self.job_id) {
            *n = n.saturating_sub(1);
        }
        drop(state);
        self.tracker.idle.notify_waiters();
    }
}

// ── Per-session inbound NAR receive store (issue #109, resumable #225) ────────

/// Outcome of [`NarReceiveStore::append`].
pub(super) enum AppendOutcome {
    /// Chunk was staged.
    Ok,
    /// Fatal: the chunk exceeded the session budget, arrived at a
    /// non-contiguous offset, or could not be staged at all. The path is now
    /// poisoned and its partial discarded - the caller aborts the job and
    /// rejects the eventual `NarUploaded` for the same path.
    Overflow,
    /// Chunk arrived for a path the session has already poisoned. Drop it.
    Poisoned,
}

/// Bulk frames one stream may queue before the session actor waits on its
/// staging task: 8 x 512 KiB.
const STAGE_QUEUE_DEPTH: usize = 8;

/// Push streams one session may hold open at once. Every open stream costs a
/// file descriptor, a staging task and up to `STAGE_QUEUE_DEPTH` queued frames,
/// none of which the byte budget sees until a chunk actually arrives. A worker
/// uploads `UPLOAD_CONCURRENCY` (4) paths per job at once, so a busy many-core
/// builder legitimately holds dozens: this is a backstop against an unbounded
/// flood, not a throughput limit, and it must stay far above real traffic.
const MAX_ACTIVE_STREAMS: usize = 256;

/// How long a stream outlives the job it belongs to. `JobCompleted` and
/// `JobFailed` ride the control lane and overtake the job's own trailing
/// `NarPush` and `NarUploaded` frames on the bulk lane, so a stream whose job
/// has just ended may still be receiving; only one that stays silent this long
/// is treated as abandoned and released.
const ENDED_STREAM_GRACE: Duration = Duration::from_secs(120);

/// What a stream's staging task accepts: the frames to append, then one request
/// to flush and report what was staged.
enum StageCmd {
    Chunk(Frame<ClientMessage>),
    Finish(oneshot::Sender<anyhow::Result<StagedFile>>),
}

struct PathState {
    /// Bytes staged for this path on this session (resumed prefix + appends).
    /// Also the offset the next chunk must carry. The stream's `stream_token`
    /// lives in the partial store's sidecar, written when its writer opened.
    staged: u64,
    tx: mpsc::Sender<StageCmd>,
    /// The grace anchor: when this stream's job ended, if it has, pushed
    /// forward by any later frame. A frame extends the grace rather than
    /// cancelling it, because the frames that follow a `JobFailed` are the
    /// abandoned streams' own queued chunks.
    ended: Option<Instant>,
    /// When a frame for this stream last arrived. The backstop for a stream
    /// that is never marked at all: a worker job task that dies without a
    /// terminal frame, or a header that arrives after its job ended.
    last_seen: Instant,
}

/// A rejected path and why, kept so the abort names the real cause. Poisons
/// carry the same grace as streams: a `NarUploaded` that `JobFailed` overtook
/// must still find its reason instead of falling through to the presigned
/// commit path.
struct Poison {
    reason: String,
    ended: Option<Instant>,
}

/// A staged direct-mode upload detached from the session's receive store so a
/// spawned task can verify and commit it off the read loop.
pub(super) struct StagedNar {
    /// The claimed `.partial`. The staging task reports its own (now stale)
    /// path, because the claim rename happens on the read loop.
    path: std::path::PathBuf,
    done: oneshot::Receiver<anyhow::Result<StagedFile>>,
}

impl StagedNar {
    /// Wait for the staging task to drain its queue and flush, then report the
    /// staged file: its claimed path, its length, and the SHA-256 the task
    /// computed as the bytes arrived.
    pub(super) async fn finish(self) -> anyhow::Result<StagedFile> {
        match self.done.await.context("NAR staging task vanished") {
            Ok(Ok(file)) => Ok(StagedFile {
                path: self.path,
                ..file
            }),
            Ok(Err(e)) | Err(e) => {
                discard_staged(&self.path).await;
                Err(e)
            }
        }
    }
}

/// Drain one push stream into its `.partial`. Owning the open file keeps every
/// chunk off the session actor: the actor only hands frames over, so a slow
/// disk applies back-pressure through the queue instead of blocking dispatch.
async fn stage_stream(mut writer: PartialWriter, mut rx: mpsc::Receiver<StageCmd>) {
    let mut failed: Option<anyhow::Error> = None;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            StageCmd::Chunk(frame) => {
                if failed.is_some() {
                    continue;
                }
                let ArchivedClientMessage::NarPush { data, offset, .. } = frame.archived() else {
                    continue;
                };
                if let Err(e) = writer.append(offset.to_native(), data.as_slice()).await {
                    failed = Some(e);
                }
            }
            StageCmd::Finish(reply) => {
                let result = match failed.take() {
                    Some(e) => Err(e),
                    None => writer.finish().await,
                };
                let _ = reply.send(result);
                return;
            }
        }
    }
}

/// Disk-backed receiver for inbound `NarPush` chunks. Each push is staged to a
/// `*.partial` file under `<base_path>/nar-partial/<peer_id>/<job_id>/<hash>` so
/// an interrupted upload can resume from a byte offset (issue #225) and a large
/// NAR no longer pins RAM. Two per-session limits preserve the #109 protection
/// against a rogue worker: `max_bytes` bounds the bytes staged on **disk**, and
/// [`MAX_ACTIVE_STREAMS`] bounds the descriptors, staging tasks and queued
/// frames that open streams hold before a single chunk is counted against the
/// budget. Either limit poisons the offending path. Keying by `peer_id` isolates
/// workers; keying by `job_id` isolates two jobs on one worker that push the
/// *same* content-addressed path concurrently - without it their interleaved
/// appends to a shared hash-keyed partial trip the contiguity check and poison a
/// valid transfer (mirrors the worker-pull `{job_id}/{hash}` namespacing).
pub(super) struct NarReceiveStore {
    store: gradient_storage::PartialStore,
    shutdown: Shutdown,
    peer_id: String,
    max_bytes: u64,
    /// A stream no longer than this is kept in memory as it is staged, so the
    /// commit can hand the bytes straight to the hot cache.
    retain_up_to: u64,
    max_streams: usize,
    ended_grace: Duration,
    active: HashMap<String, PathState>,
    /// How long a stream may receive nothing at all before a sweep releases it
    /// regardless of any mark. Sweeps run from [`Self::note_header`] and
    /// [`Self::forget_job`] only, with no timer behind them, so a silent stream
    /// outlives this on a session that sees no further header or job end. Zero
    /// disables the backstop, as it does for [`gradient_storage::PartialStore::gc`],
    /// whose TTL this is.
    idle_timeout: Duration,
    poisoned: BTreeMap<String, Poison>,
    commits: Arc<CommitTracker>,
}

/// In-memory key isolating a path's staging state per job, so two jobs pushing
/// the same store path never share an `active`/`poisoned` entry. The unit
/// separator can appear in neither a job id nor a store path.
fn state_key(job_id: &str, store_path: &str) -> String {
    format!("{job_id}\u{1f}{store_path}")
}

impl NarReceiveStore {
    pub(super) fn new(
        root: std::path::PathBuf,
        peer_id: &str,
        ttl: std::time::Duration,
        max_bytes: u64,
        retain_up_to: u64,
        shutdown: Shutdown,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            store: gradient_storage::PartialStore::new(root, ttl)?,
            shutdown,
            peer_id: peer_id.to_owned(),
            max_bytes,
            retain_up_to,
            max_streams: MAX_ACTIVE_STREAMS,
            ended_grace: ENDED_STREAM_GRACE,
            idle_timeout: ttl,
            active: HashMap::new(),
            poisoned: BTreeMap::new(),
            commits: Arc::new(CommitTracker::default()),
        })
    }

    /// Shrink the limits so a test can reach them without opening hundreds of
    /// files or waiting out a grace. Takes `&mut self` so a test can tighten
    /// them between phases.
    #[cfg(test)]
    pub(super) fn set_limits(
        &mut self,
        max_streams: usize,
        ended_grace: Duration,
        idle_timeout: Duration,
    ) {
        self.max_streams = max_streams;
        self.ended_grace = ended_grace;
        self.idle_timeout = idle_timeout;
    }

    fn key(&self, job_id: &str, hash: &str) -> String {
        format!("{}/{}/{}", self.peer_id, job_id, hash)
    }

    /// Stop the staging task for `sk` and wait until it has drained its queue
    /// and closed the file, so a truncate or unlink of that partial cannot race
    /// a write from the stream being replaced.
    async fn retire(&mut self, sk: &str) {
        let Some(state) = self.active.remove(sk) else {
            return;
        };

        let (tx, rx) = oneshot::channel();
        if state.tx.send(StageCmd::Finish(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Mark a job's push streams and poisons as ended. The worker drops the
    /// uploads still in flight when a job fails or is aborted, so without this
    /// every such job would strand up to `UPLOAD_CONCURRENCY - 1` streams, each
    /// holding a descriptor, a staging task and a slot under `max_streams`, for
    /// the life of the session. They are marked rather than closed because
    /// `JobCompleted` overtakes the job's own trailing bulk frames:
    /// [`ENDED_STREAM_GRACE`] gives those time to land.
    /// The commits this session's jobs still owe the index. Cloned out so a
    /// completion can wait for its own job off the read loop.
    pub(super) fn commits(&self) -> Arc<CommitTracker> {
        Arc::clone(&self.commits)
    }

    pub(super) async fn forget_job(&mut self, job_id: &str) {
        let prefix = format!("{job_id}\u{1f}");
        let now = Instant::now();
        for state in self
            .active
            .iter_mut()
            .filter(|(sk, _)| sk.starts_with(&prefix))
            .map(|(_, state)| state)
        {
            state.ended.get_or_insert(now);
        }
        for poison in self
            .poisoned
            .iter_mut()
            .filter(|(sk, _)| sk.starts_with(&prefix))
            .map(|(_, poison)| poison)
        {
            poison.ended.get_or_insert(now);
        }

        self.sweep_stale().await;
    }

    /// Release every stream that is past its grace or has simply gone silent,
    /// discarding its partial: nothing can finish a stream whose job is gone.
    /// Driven by the next header or job end on this session, never by a timer.
    /// The `Finish` round-trip per stream is a task wake-up, not a transfer: a
    /// stream selected here has had no frame for at least the grace, so its
    /// queue is empty and the read loop is not held up.
    async fn sweep_stale(&mut self) {
        let now = Instant::now();
        let idle_out = |since: Instant| {
            !self.idle_timeout.is_zero() && now.duration_since(since) >= self.idle_timeout
        };
        let stale: Vec<String> = self
            .active
            .iter()
            .filter(|(_, state)| {
                state
                    .ended
                    .is_some_and(|at| now.duration_since(at) >= self.ended_grace)
                    || idle_out(state.last_seen)
            })
            .map(|(sk, _)| sk.clone())
            .collect();

        for sk in stale {
            let Some((job_id, store_path)) = sk.split_once('\u{1f}') else {
                continue;
            };
            let (job_id, store_path) = (job_id.to_owned(), store_path.to_owned());
            debug!(%job_id, %store_path, "releasing a stale push stream");
            self.finish(&job_id, &store_path).await;
        }

        let grace = self.ended_grace;
        self.poisoned
            .retain(|_, p| p.ended.is_none_or(|at| now.duration_since(at) < grace));
    }

    /// Record the push stream's token, open its partial, and return how many
    /// bytes are already staged for it (0 on token mismatch / nothing on disk).
    /// Clears any stale poison so a fresh attempt can proceed. A stream that
    /// cannot be opened, or that would exceed [`MAX_ACTIVE_STREAMS`], poisons
    /// the path with its reason; callers check [`Self::poison_reason`] rather
    /// than treating the `0` as a fresh start. The cap is tested after `retire`
    /// has removed this path's own entry, so re-heading a stream that is already
    /// open is never rejected for being one over the ceiling.
    pub(super) async fn note_header(&mut self, job_id: &str, store_path: &str, token: &str) -> u64 {
        let sk = state_key(job_id, store_path);
        self.poisoned.remove(&sk);
        self.retire(&sk).await;

        let Some(hash) = store_hash(store_path) else {
            return 0;
        };

        self.sweep_stale().await;
        if self.active.len() >= self.max_streams {
            let reason = format!(
                "too many open NAR push streams on this session (limit {})",
                self.max_streams,
            );
            warn!(%store_path, "{reason}; poisoning path");
            self.poison(job_id, store_path, hash, reason).await;
            return 0;
        }

        let key = self.key(job_id, hash);
        let received = self.store.received_len(&key, token).await.unwrap_or(0);
        let writer = match self
            .store
            .open_writer(&key, token, received, self.retain_up_to)
            .await
        {
            Ok(writer) => writer,
            Err(e) => {
                let reason = format!("failed to open the staged partial for {store_path}: {e}");
                warn!(%store_path, error = %e, "failed to open staged partial; poisoning path");
                self.poison(job_id, store_path, hash, reason).await;
                return 0;
            }
        };

        let (tx, rx) = mpsc::channel(STAGE_QUEUE_DEPTH);
        self.shutdown.spawn(stage_stream(writer, rx));
        self.active.insert(
            sk,
            PathState {
                staged: received,
                tx,
                ended: None,
                last_seen: Instant::now(),
            },
        );
        received
    }

    /// Hand a chunk to this path's staging task. A push stream is append-only
    /// once opened: `offset` must equal the bytes already staged, so restarting
    /// a stream at 0 takes a fresh `NarStreamHeader` (which re-opens the writer,
    /// truncating only when the stream token changed) rather than a bare chunk.
    /// A gap is fatal here rather than at commit time so the worker is aborted
    /// before it streams the rest of a NAR that can never be stored. Opens a
    /// token-less stream for legacy pushes that skip the header.
    pub(super) async fn append(
        &mut self,
        job_id: &str,
        store_path: &str,
        offset: u64,
        frame: Frame<ClientMessage>,
    ) -> AppendOutcome {
        let sk = state_key(job_id, store_path);
        if self.poisoned.contains_key(&sk) {
            return AppendOutcome::Poisoned;
        }

        let Some(hash) = store_hash(store_path) else {
            return AppendOutcome::Poisoned;
        };

        let len = {
            let ArchivedClientMessage::NarPush { data, .. } = frame.archived() else {
                warn!(%store_path, "non-NarPush frame routed to the NAR receive store");
                return AppendOutcome::Poisoned;
            };
            data.len() as u64
        };

        if !self.active.contains_key(&sk) {
            self.note_header(job_id, store_path, "").await;
        }
        let Some(state) = self.active.get(&sk) else {
            return AppendOutcome::Overflow;
        };

        let tx = state.tx.clone();
        let staged = state.staged;
        if offset != staged {
            let reason = format!(
                "NarPush for {store_path} at offset {offset} follows {staged} staged bytes"
            );
            warn!(%store_path, offset, staged, "non-contiguous NarPush; poisoning path");
            self.poison(job_id, store_path, hash, reason).await;
            return AppendOutcome::Overflow;
        }

        let total: u64 = self.active.values().map(|s| s.staged).sum();
        if total.saturating_add(len) > self.max_bytes {
            let reason = format!(
                "NAR upload for {store_path} exceeds the staged-partial budget ({} bytes)",
                self.max_bytes,
            );
            self.poison(job_id, store_path, hash, reason).await;
            return AppendOutcome::Overflow;
        }

        if tx.send(StageCmd::Chunk(frame)).await.is_err() {
            let reason = format!("the staging task for {store_path} is gone");
            warn!(%store_path, "NAR staging task gone; poisoning path");
            self.poison(job_id, store_path, hash, reason).await;
            return AppendOutcome::Overflow;
        }

        if let Some(s) = self.active.get_mut(&sk) {
            let now = Instant::now();
            s.staged += len;
            s.last_seen = now;
            // A frame after the job ended extends the grace, it does not cancel
            // it: an abandoned stream keeps flushing the chunks the worker had
            // already queued, and would otherwise never be swept.
            s.ended = s.ended.map(|_| now);
        }
        AppendOutcome::Ok
    }

    async fn poison(&mut self, job_id: &str, store_path: &str, hash: &str, reason: String) {
        let sk = state_key(job_id, store_path);
        self.retire(&sk).await;
        let key = self.key(job_id, hash);
        let _ = self.store.discard(&key).await;
        self.poisoned.insert(
            sk,
            Poison {
                reason,
                ended: None,
            },
        );
    }

    /// Detach the staged stream for `store_path` so a spawned task can commit
    /// it without borrowing the session's receive store. Returns `None` when no
    /// direct-mode stream is open (a presigned upload). The partial is *claimed*
    /// under a unique key here, synchronously on the read loop, and only then is
    /// the staging task asked to flush: the commit runs detached and can lag
    /// behind the commit semaphore, and the same content-addressed `.drv` is
    /// pushed repeatedly across an eval's closure walk. A later push of the same
    /// hash resets the shared `{peer}/{job}/{hash}` partial (token-mismatch
    /// discard / `offset == 0` truncate), so a claim taken off the read loop
    /// could be raced by the very next header. The rename does not disturb the
    /// staging task: it writes through an open handle to the same file.
    pub(super) async fn take_staged(
        &mut self,
        job_id: &str,
        store_path: &str,
    ) -> Option<StagedNar> {
        let hash = store_hash(store_path)?;
        let state = self.active.remove(&state_key(job_id, store_path))?;
        let base_key = self.key(job_id, hash);
        let key = match self.store.detach(&base_key).await {
            Ok(Some(claim)) => claim,
            Ok(None) => base_key,
            Err(e) => {
                warn!(%store_path, error = %e, "failed to claim staged partial; using shared key");
                // The commit adopts (renames away) the shared partial, and the
                // TTL sweep only ever sees a `.partial`, so drop the sidecar now
                // or it is orphaned for good.
                let _ = self.store.discard_token(&base_key).await;
                base_key
            }
        };

        let (tx, done) = oneshot::channel();
        if state.tx.send(StageCmd::Finish(tx)).await.is_err() {
            warn!(%store_path, "NAR staging task gone before the stream was finished");
        }
        Some(StagedNar {
            path: self.store.path(&key),
            done,
        })
    }

    /// Drop the staged partial and per-path state after a successful commit.
    pub(super) async fn finish(&mut self, job_id: &str, store_path: &str) {
        self.retire(&state_key(job_id, store_path)).await;
        if let Some(hash) = store_hash(store_path) {
            let key = self.key(job_id, hash);
            let _ = self.store.discard(&key).await;
        }
    }

    /// Why this path was poisoned, if it was, for the abort the worker is sent.
    pub(super) fn poison_reason(&self, job_id: &str, store_path: &str) -> Option<&str> {
        self.poisoned
            .get(&state_key(job_id, store_path))
            .map(|p| p.reason.as_str())
    }

    /// Forget the poison flag and discard any partial for `store_path` so a
    /// later, well-formed retry of the same path can proceed.
    pub(super) async fn clear_poison(&mut self, job_id: &str, store_path: &str) {
        self.poisoned.remove(&state_key(job_id, store_path));
        self.finish(job_id, store_path).await;
    }
}

/// Extract and validate the 32-char store-hash from a `/nix/store/<hash>-name`
/// path. Returns `None` for anything malformed.
fn store_hash(store_path: &str) -> Option<&str> {
    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()?;
    (hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(hash)
}

// ── DispatchContext NAR handlers ──────────────────────────────────────────────

impl<'a> DispatchContext<'a> {
    /// Open (or resume) a push stream and tell the worker how many compressed
    /// bytes are already staged so it can seek its regenerated zstd stream.
    pub(super) async fn on_push_stream_header(
        &mut self,
        job_id: String,
        store_path: String,
        _total_bytes: Option<u64>,
        stream_token: String,
        nar: &mut NarReceiveStore,
    ) {
        let received = nar.note_header(&job_id, &store_path, &stream_token).await;
        if let Some(reason) = nar.poison_reason(&job_id, &store_path) {
            // Answering with `received_bytes: 0` here would have the worker
            // stream the whole NAR into the poisoned arm, to be rejected only at
            // `NarUploaded` with a reason naming neither cause.
            let reason = format!("NAR upload for {store_path} cannot be staged: {reason}");
            error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "refusing push stream");
            self.abort_job(&job_id, reason).await;
            return;
        }

        trace!(peer_id = %self.peer_id, %job_id, %store_path, received, "NarStreamHeader (push)");
        let _ = send_server_msg(
            self.writer,
            &ServerMessage::NarPushResume {
                job_id,
                store_path,
                received_bytes: received,
            },
        )
        .await;
    }

    pub(super) async fn on_nar_push(
        &mut self,
        job_id: &str,
        store_path: &str,
        frame: Frame<ClientMessage>,
        nar: &mut NarReceiveStore,
    ) {
        let (offset, len, is_final) = {
            let ArchivedClientMessage::NarPush {
                data,
                offset,
                is_final,
                ..
            } = frame.archived()
            else {
                return;
            };
            (offset.to_native(), data.len(), *is_final)
        };

        trace!(peer_id = %self.peer_id, %job_id, %store_path, offset, is_final, bytes = len, "NarPush");
        if len == 0 {
            return;
        }
        match nar.append(job_id, store_path, offset, frame).await {
            AppendOutcome::Ok => {}
            AppendOutcome::Overflow => {
                let reason = nar.poison_reason(job_id, store_path).map_or_else(
                    || format!("NAR upload for {store_path} rejected at offset {offset}"),
                    |reason| format!("NAR upload for {store_path} rejected: {reason}"),
                );
                warn!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "poisoning NAR path");
                self.abort_job(job_id, reason).await;
            }
            AppendOutcome::Poisoned => {
                debug!(peer_id = %self.peer_id, %job_id, %store_path, "discarding NarPush chunk for poisoned path");
            }
        }
        // The partial is held until `on_nar_uploaded` arrives; that handler
        // commits it to `nar_storage` and records the metadata atomically so
        // we never end up with a `cached_path` row claiming bytes that
        // aren't actually stored.
    }

    /// Apply the worker's NAR upload metadata.
    ///
    /// For direct-mode pushes (preceded by a `NarStreamHeader` + `NarPush`
    /// chunks), the staged `*.partial` is validated against the reported
    /// `file_size`, written to `nar_storage`, and only then is
    /// `mark_nar_stored` invoked. Any failure aborts the job with
    /// [`ServerMessage::AbortJob`] so the build is marked failed and the
    /// scheduler does not advertise the path as cached.
    ///
    /// For S3 / presigned uploads (no preceding push stream), the worker has
    /// already PUT the bytes directly to object storage, so the object is
    /// HEADed and its size compared against the reported `file_size` before
    /// any metadata is recorded. Skipping that check would let a failed or
    /// truncated PUT create a `cached_path` row pointing at a missing or
    /// corrupt object - the zombie class the demote/reconcile machinery
    /// exists to repair.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the wire-protocol message fields; refactor tracked in #503"
    )]
    pub(super) async fn on_nar_uploaded(
        &mut self,
        job_id: String,
        store_path: String,
        file_hash: String,
        file_size: u64,
        nar_size: u64,
        nar_hash: String,
        references: Vec<String>,
        deriver: Option<String>,
        ca: Option<String>,
        nar: &mut NarReceiveStore,
    ) {
        trace!(peer_id = %self.peer_id, %job_id, %store_path, %file_hash, file_size, nar_size, %nar_hash, ?deriver, ?ca, "NarUploaded");

        // Reject any NarUploaded for a path whose chunked transfer was rejected
        // mid-stream. Without this guard `mark_nar_stored` would record a
        // `cached_path` row whose bytes never reached `nar_storage` - leaving
        // the path "cached" in the DB and undeliverable on the next download.
        if let Some(poisoned) = nar.poison_reason(&job_id, &store_path) {
            let reason = format!("NarUploaded for {store_path} rejected: {poisoned}");
            nar.clear_poison(&job_id, &store_path).await;
            warn!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "rejecting NarUploaded for poisoned path");
            self.abort_job(&job_id, reason).await;
            return;
        }

        let Some(hash) = store_hash(&store_path) else {
            let reason = format!("NarUploaded for malformed store path {store_path}");
            error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "NarUploaded for malformed store path");
            self.abort_job(&job_id, reason).await;
            return;
        };

        // Resolve the owning project here, on the read loop, while the job is
        // still active; re-resolving inside the detached task would race the
        // eviction that `JobCompleted` triggers. The tracker alone is not
        // enough, though: `JobCompleted` rides the control writer lane and
        // overtakes this job's own trailing `NarUploaded` frames on the bulk
        // lane, so the job can already be gone. Fall back to the durable
        // dispatch row rather than committing with no cache claim - that leaves
        // a `cached_path` row the narinfo gate 404s forever.
        let project_id = match self.scheduler.project_for_job(&job_id).await {
            Some(project_id) => Some(project_id),
            None => {
                super::nar::project_for_dispatched_job(&self.state.worker_db, self.peer_id, &job_id)
                    .await
            }
        };
        if project_id.is_none() {
            warn!(
                peer_id = %self.peer_id, %job_id, %store_path,
                "no owning project for this NAR; it will be stored with no cache claim"
            );
        }

        // The commit reads the whole staged NAR and writes it to `nar_storage`
        // (an S3 upload on object-store backends). Inline it froze this
        // session's read loop for the duration, so the worker's own bounded
        // sends backed up, the worker stopped reading, and every concurrent
        // transfer on the connection stalled into its send timeout
        // ("WebSocket send stalled on final NarPush"). Detach the staged
        // stream synchronously, then commit on a bounded spawned task.
        let staged = nar.take_staged(&job_id, &store_path).await;
        // Claimed before the spawn, so a `JobCompleted` that reaches the read loop
        // between here and the task's first poll already sees the commit owed.
        let guard = nar.commits().start(&job_id);
        let writer = self.writer.clone();
        let state = Arc::clone(self.state);
        let scheduler = Arc::clone(self.scheduler);
        let peer_id = self.peer_id.to_owned();
        let hash = hash.to_owned();
        let shutdown = state.shutdown.clone();
        shutdown.spawn(async move {
            commit_uploaded_nar(CommitUploadedNar {
                writer,
                state,
                scheduler,
                peer_id,
                job_id,
                project_id,
                store_path,
                hash,
                file_hash,
                file_size,
                nar_size,
                nar_hash,
                references,
                deriver,
                ca,
                staged,
                guard,
            })
            .await;
        });
    }

    /// Send `AbortJob` to the worker. Used when a NAR upload cannot be
    /// committed safely - the worker stops the job and replies with
    /// `JobFailed`, which the scheduler turns into a failed build.
    async fn abort_job(&mut self, job_id: &str, reason: String) {
        abort_job_msg(self.writer, job_id, reason).await;
    }
}

/// Everything a detached NAR commit needs, owned so the session read loop is
/// free the moment the task is spawned.
struct CommitUploadedNar {
    writer: ProtoWriter,
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    peer_id: String,
    job_id: String,
    project_id: Option<gradient_types::ids::ProjectId>,
    store_path: String,
    hash: String,
    file_hash: String,
    file_size: u64,
    nar_size: u64,
    nar_hash: String,
    references: Vec<String>,
    deriver: Option<String>,
    ca: Option<String>,
    staged: Option<StagedNar>,
    /// Released when this task ends, however it ends.
    guard: CommitGuard,
}

/// What a relayed commit left behind for the index and the hot cache.
struct Relayed {
    confirmed: bool,
    hot: HotUpdate,
}

enum HotUpdate {
    Insert(bytes::Bytes),
    Invalidate,
    Keep,
}

/// Detached storage commit plus DB effects for one `NarUploaded`.
async fn commit_uploaded_nar(c: CommitUploadedNar) {
    let relayed = match c.staged {
        Some(staged) => {
            match commit_relayed(
                &c.writer,
                &c.state,
                &c.scheduler,
                &c.peer_id,
                &c.job_id,
                &c.store_path,
                &c.hash,
                &c.file_hash,
                c.file_size,
                staged,
            )
            .await
            {
                Some(relayed) => relayed,
                None => return c.guard.fail(),
            }
        }
        None => {
            if !commit_presigned(
                &c.writer,
                &c.state,
                &c.scheduler,
                &c.peer_id,
                &c.job_id,
                &c.store_path,
                &c.hash,
                &c.file_hash,
                c.file_size,
            )
            .await
            {
                return c.guard.fail();
            }

            Relayed {
                confirmed: true,
                hot: HotUpdate::Invalidate,
            }
        }
    };

    let file_size_i64 = c.file_size as i64;
    let nar_record = NarUploadRecord {
        file_hash: &c.file_hash,
        file_size: file_size_i64,
        nar_size: c.nar_size as i64,
        nar_hash: &c.nar_hash,
        references: &c.references,
        deriver: c.deriver.as_deref(),
        ca: c.ca.as_deref(),
        confirmed: relayed.confirmed,
    };
    match mark_nar_stored(&c.state, c.project_id, &c.store_path, &nar_record).await {
        Ok(()) => {
            match relayed.hot {
                HotUpdate::Insert(bytes) => c.state.nar_storage.hot().insert(&c.hash, bytes),
                HotUpdate::Invalidate => c.state.nar_storage.hot().invalidate(&c.hash),
                HotUpdate::Keep => {}
            }

            if !relayed.confirmed
                && let Some(staged) = c.state.nar_storage.staged()
            {
                staged.wake();
            }
        }
        // The last step of the upload is the only one that was allowed to fail
        // quietly, and it is the one that decides whether the cache has the path:
        // the bytes are in storage and the index never learns it, so the anchor
        // reaches terminal success against an output nothing serves. A full disk
        // or a DB error here is the same class of transient server-side failure as
        // the staged-read and integrity checks above, and it fails the build the
        // same way rather than leaving the graph to discover the lie later.
        Err(e) => {
            let reason = format!("failed to record {} in the cache index: {e}", c.store_path);
            error!(peer_id = %c.peer_id, job_id = %c.job_id, store_path = %c.store_path, %reason, "NAR commit failed");
            fail_build_transient(&c.writer, &c.scheduler, &c.peer_id, &c.job_id, reason).await;
            return c.guard.fail();
        }
    }
    if let Err(e) = record_nar_push_metric(&c.state, c.project_id, file_size_i64).await {
        debug!(error = %e, "failed to record cache metric for NarUploaded");
    }
}

/// Commit a direct-mode (relayed) push: wait for the staging task to flush,
/// check the length and hash it reports against the ones the worker reported,
/// then put the staged file where it is served from. Returns `None` (after
/// failing the build transiently) if any step fails.
#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
async fn commit_relayed(
    writer: &ProtoWriter,
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    peer_id: &str,
    job_id: &str,
    store_path: &str,
    hash: &str,
    file_hash: &str,
    file_size: u64,
    staged: StagedNar,
) -> Option<Relayed> {
    let file = match staged.finish().await {
        Ok(file) => file,
        Err(e) => {
            let reason = format!("failed to stage NAR: {e}");
            error!(%peer_id, %job_id, %store_path, error = %e, "NAR staging failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            return None;
        }
    };

    if file.len != file_size {
        let reason = format!(
            "staged NAR size {} does not match reported file_size {file_size}",
            file.len
        );
        error!(%peer_id, %job_id, %store_path, %reason, "NAR upload integrity check failed");
        fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
        discard_staged(&file.path).await;
        return None;
    }

    if !gradient_storage::file_hash_matches(file_hash, &file.sha256) {
        let reason = format!("NAR content verification failed: staged bytes are not {file_hash}");
        error!(%peer_id, %job_id, %store_path, %reason, "NAR upload integrity check failed");
        fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
        discard_staged(&file.path).await;
        return None;
    }

    match crate::ingest::nar_write_needed(&state.worker_db, &state.nar_storage, hash, file_hash)
        .await
    {
        Ok(WriteNeeded::Write) => match place_relayed(&state.nar_storage, hash, &file.path).await {
            Ok(confirmed) => {
                let hot = match file.bytes {
                    Some(bytes) if state.nar_storage.hot().admits(bytes.len() as u64) => {
                        HotUpdate::Insert(bytes)
                    }
                    _ => HotUpdate::Invalidate,
                };
                trace!(%peer_id, %job_id, %store_path, file_size, confirmed, "NAR placed");
                Some(Relayed { confirmed, hot })
            }
            Err(e) => {
                let reason = format!("failed to write NAR to storage: {e}");
                error!(%peer_id, %job_id, %store_path, error = %e, "place_relayed failed");
                fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
                None
            }
        },
        Ok(WriteNeeded::Stored) => {
            discard_staged(&file.path).await;
            Some(Relayed {
                confirmed: true,
                hot: HotUpdate::Keep,
            })
        }
        Ok(WriteNeeded::Staged) => {
            discard_staged(&file.path).await;
            Some(Relayed {
                confirmed: false,
                hot: HotUpdate::Keep,
            })
        }
        Err(e) => {
            let reason = format!("failed to check for a stored NAR: {e}");
            error!(%peer_id, %job_id, %store_path, error = %e, "NAR idempotency check failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            None
        }
    }
}

/// Put a verified claim where it is served from: the object store by rename on
/// the local backend, `nar-staged/` on an S3 backend with staging so the
/// uploader moves it on, or straight into S3 where there is no staging.
/// Returns whether the object is in storage now.
async fn place_relayed(
    store: &gradient_storage::NarStore,
    hash: &str,
    claim: &std::path::Path,
) -> anyhow::Result<bool> {
    if store.local_base().is_none()
        && let Some(staged) = store.staged()
    {
        staged.adopt(hash, claim).await?;
        return Ok(false);
    }

    store.adopt_file(hash, claim).await?;
    Ok(true)
}

/// Drop a staged file `nar_storage` did not adopt. A leftover is reclaimed by
/// the partial-store TTL sweep anyway, so a failure here is only logged.
async fn discard_staged(path: &std::path::Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => debug!(path = %path.display(), error = %e, "failed to remove staged NAR"),
    }
}

/// Commit a presigned (S3) upload: the worker already PUT the bytes directly,
/// so [`NarStore::verify`] confirms the object exists at the reported
/// `file_size`; with `nar_verify_digest` enabled it also rehashes the object
/// against `file_hash`. Returns `false` (after failing the build transiently)
/// on any mismatch.
#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
async fn commit_presigned(
    writer: &ProtoWriter,
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    peer_id: &str,
    job_id: &str,
    store_path: &str,
    hash: &str,
    file_hash: &str,
    file_size: u64,
) -> bool {
    let rehash = state.config.storage.nar_verify_digest;
    match state
        .nar_storage
        .verify(hash, file_hash, file_size, rehash)
        .await
    {
        Ok(()) => true,
        Err(e) => {
            let reason = format!("presigned NAR upload verification failed: {e}");
            error!(%peer_id, %job_id, %store_path, %reason, "presigned NAR upload integrity check failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            false
        }
    }
}

async fn abort_job_msg(writer: &ProtoWriter, job_id: &str, reason: String) {
    let _ = send_server_msg(
        writer,
        &ServerMessage::AbortJob {
            job_id: job_id.to_owned(),
            reason,
        },
    )
    .await;
}

/// A transient server-side NAR storage failure (staged-read or
/// `nar_storage` write). Stop the worker and mark the build
/// `FailedTransient` directly so the dispatcher re-queues it - a bare
/// abort would be reported by the worker as a permanent failure and
/// never retry. The connection is untouched; only this build fails.
async fn fail_build_transient(
    writer: &ProtoWriter,
    scheduler: &Arc<Scheduler>,
    peer_id: &str,
    job_id: &str,
    reason: String,
) {
    abort_job_msg(writer, job_id, reason.clone()).await;
    if let Err(e) = scheduler
        .handle_job_failed(
            peer_id,
            job_id,
            &reason,
            gradient_types::proto::BuildFailureKind::Transient,
            &[],
        )
        .await
    {
        error!(%peer_id, %job_id, error = %e, "fail_build_transient: handle_job_failed failed");
    }
}

// ── NAR serving ───────────────────────────────────────────────────────────────

/// Which message a failed transfer sends: [`ServerMessage::NarUnavailable`]
/// before any bytes have streamed, or [`ServerMessage::NarAbort`] mid-stream.
enum FailKind {
    Unavailable,
    Abort,
}

/// Send the message matching `kind` and return the error every call site
/// returns. Centralizes the abort-and-return idiom `serve_nar_request` used to
/// repeat at every error exit.
async fn fail_transfer(
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    kind: FailKind,
    reason: String,
) -> anyhow::Error {
    match kind {
        FailKind::Unavailable => {
            let _ = send_server_msg(
                writer,
                &ServerMessage::NarUnavailable {
                    job_id: job_id.to_owned(),
                    store_path: store_path.to_owned(),
                    reason: reason.clone(),
                },
            )
            .await;
        }
        FailKind::Abort => {
            let _ = send_server_msg(
                writer,
                &ServerMessage::NarAbort {
                    job_id: job_id.to_owned(),
                    store_path: store_path.to_owned(),
                    reason: reason.clone(),
                },
            )
            .await;
        }
    }
    anyhow::anyhow!(reason)
}

/// Stream a single requested NAR from `nar_storage` to the worker.
///
/// Hardening notes:
/// - The initial storage open is wrapped in `storage_open_timeout`. A stalled
///   backend (e.g. S3 hung TCP) used to silently consume the dispatch loop's
///   600 s waiter ceiling; now it surfaces as a `NarUnavailable` within the
///   open timeout.
/// - The chunked send path uses [`ProtoWriter`], which bounds per-chunk send
///   waits via the queue + `send_chunk_timeout` configured at split time.
///   A stalled peer is detected as `Err(())` from `send_server_msg` and
///   triggers a best-effort `NarAbort`.
/// - The body is read from `object_store`'s streaming API - no full file is
///   ever held in memory. Chunks are coalesced/split to `BULK_CHUNK_SIZE`.
/// - Per-chunk read from the storage stream is also bounded so a backend that
///   sends the first byte and then hangs cannot pin the task indefinitely.
pub(super) async fn serve_nar_request(
    state: &Arc<ServerState>,
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    resume_from: u64,
    client_token: Option<&str>,
) -> anyhow::Result<()> {
    let proto_cfg = &state.config.proto;
    let storage_open_timeout = Duration::from_secs(proto_cfg.nar_storage_open_timeout_secs);
    let chunk_read_timeout = Duration::from_secs(proto_cfg.nar_send_chunk_timeout_secs);

    let Some(hash) = store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
    else {
        let reason = format!("invalid store path: {store_path}");
        return Err(fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await);
    };

    let open = |offset: u64| async move {
        tokio::time::timeout(storage_open_timeout, state.nar_storage.open(hash, offset)).await
    };

    let mut source = match open(resume_from).await {
        Ok(Ok(Some(source))) => source,
        Ok(Ok(None)) => {
            invalidate_cached_path(state, hash, store_path).await;
            let reason = format!("NAR not found in cache for {store_path}");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
        Ok(Err(e)) => {
            let reason = format!("nar_storage.open({hash}) failed: {e}");
            error!(%store_path, error = %e, "NAR storage read error");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
        Err(_) => {
            let reason = format!(
                "nar_storage.open({hash}) timed out after {}s",
                storage_open_timeout.as_secs()
            );
            warn!(%store_path, "NAR storage open timed out");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
    };

    let size = source.size();

    // The stored `.nar.zst` is immutable per hash, so the pull token is just
    // its size. A worker resuming with a stale token (or claiming more bytes
    // than exist) restarts from 0; the `NarStreamHeader.total_bytes` lets the
    // worker truncate its `.partial` accordingly.
    let server_token = format!("len-{size}");
    let token_mismatch = client_token.is_some_and(|t| t != server_token);
    let mut start = resume_from;
    if resume_from > size || token_mismatch {
        match open(0).await {
            Ok(Ok(Some(s))) => {
                source = s;
                start = 0;
            }
            _ => {
                let reason = format!("failed to reopen {store_path} for fresh transfer");
                return Err(fail_transfer(
                    writer,
                    job_id,
                    store_path,
                    FailKind::Unavailable,
                    reason,
                )
                .await);
            }
        }
    }

    send_server_msg(
        writer,
        &ServerMessage::NarStreamHeader {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            total_bytes: size,
            stream_token: server_token,
        },
    )
    .await
    .ok();

    let mut stream = match source {
        NarSource::Hot(bytes) => {
            let tail = bytes.slice(usize::try_from(start).unwrap_or(0)..);
            futures::stream::once(async move { Ok(tail) }).boxed()
        }
        NarSource::Stream { stream, .. } => stream,
    };

    let mut buf: Vec<u8> = Vec::with_capacity(BULK_CHUNK_SIZE);
    let mut offset: u64 = start;
    let mut total: u64 = 0;
    let mut chunks_sent: u64 = 0;

    loop {
        let next = tokio::time::timeout(chunk_read_timeout, stream.next()).await;
        let item = match next {
            Ok(Some(x)) => x,
            Ok(None) => break,
            Err(_) => {
                let reason = format!(
                    "NAR storage read stalled > {}s mid-transfer",
                    chunk_read_timeout.as_secs()
                );
                warn!(%store_path, "NAR storage read stall");
                return Err(
                    fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await,
                );
            }
        };
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                let reason = format!("NAR storage stream error: {e}");
                error!(%store_path, error = %e, "NAR storage stream error");
                return Err(
                    fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await,
                );
            }
        };

        let mut slice = &bytes[..];
        while !slice.is_empty() {
            let want = BULK_CHUNK_SIZE - buf.len();
            let take = slice.len().min(want);
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == BULK_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(BULK_CHUNK_SIZE));
                let chunk_len = chunk.len() as u64;
                if send_server_msg(
                    writer,
                    &ServerMessage::NarPush {
                        job_id: job_id.to_owned(),
                        store_path: store_path.to_owned(),
                        data: chunk,
                        offset,
                        is_final: false,
                    },
                )
                .await
                .is_err()
                {
                    let reason = format!("WebSocket send stalled mid-NarPush at offset {offset}");
                    return Err(
                        fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await,
                    );
                }
                offset += chunk_len;
                total += chunk_len;
                chunks_sent += 1;
            }
        }
    }

    let final_len = buf.len() as u64;
    if send_server_msg(
        writer,
        &ServerMessage::NarPush {
            job_id: job_id.to_owned(),
            store_path: store_path.to_owned(),
            data: buf,
            offset,
            is_final: true,
        },
    )
    .await
    .is_err()
    {
        let reason = format!("WebSocket send stalled on final NarPush at offset {offset}");
        return Err(fail_transfer(writer, job_id, store_path, FailKind::Abort, reason).await);
    }
    total += final_len;
    chunks_sent += 1;

    trace!(%store_path, bytes = total, chunks = chunks_sent, "NarRequest served (streaming)");
    Ok(())
}

/// Purge a `cached_path` row whose NAR is no longer in `nar_storage`.
///
/// Deletes the stale artifact and clears `derivation_output.is_cached` /
/// `cached_path` so the next `CacheQuery` stops claiming the path is available -
/// letting the next build either rebuild from source or pick the path up from a
/// configured upstream. The derivation graph is untouched.
async fn invalidate_cached_path(state: &Arc<ServerState>, hash: &str, store_path: &str) {
    if awaiting_upload(state, hash).await {
        warn!(%hash, %store_path, "NAR not on this instance yet; the row is a fresh staged upload");
        return;
    }

    state.nar_storage.hot().invalidate(hash);
    match state
        .graph
        .demote(Demotion::MissingNar {
            hash: hash.to_owned(),
        })
        .await
    {
        Ok(_) => warn!(
            %hash,
            %store_path,
            "self-heal: NAR missing from storage; cached_path demoted so the path will be rebuilt"
        ),
        Err(e) => {
            warn!(%hash, %store_path, error = %e, "self-heal: failed to demote cached output")
        }
    }
}

/// An unconfirmed row younger than the upload grace is a NAR another instance
/// has staged and not yet uploaded, not a missing one.
async fn awaiting_upload(state: &Arc<ServerState>, hash: &str) -> bool {
    use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
    use sea_orm::{ColumnTrait as _, EntityTrait as _, QueryFilter as _};

    let grace = chrono::Duration::hours(state.config.storage.nar_upload_grace_hours.max(0));
    match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.worker_db)
        .await
    {
        Ok(Some(row)) => !row.confirmed && row.created_at > gradient_types::now() - grace,
        _ => false,
    }
}

#[cfg(test)]
mod commit_tracker_tests {
    use super::CommitTracker;
    use gradient_util::shutdown::Shutdown;
    use std::sync::Arc;
    use std::time::Duration;

    /// A completion must not pass a commit that is still running. The claim is
    /// taken on the read loop before the commit task is spawned, so there is no
    /// window in which `settle` can see zero for a commit that is owed.
    #[tokio::test]
    async fn settle_waits_for_a_commit_that_is_still_in_flight() {
        let tracker = Arc::new(CommitTracker::default());
        let guard = tracker.start("build:1");

        let waiter = {
            let tracker = Arc::clone(&tracker);
            Shutdown::new().spawn(async move { tracker.settle("build:1").await })
        };

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "the completion passed an owed commit"
        );

        drop(guard);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .expect("the completion never woke")
                .expect("waiter panicked"),
            "a commit that landed settles clean"
        );
    }

    /// A commit that failed has already failed the build, so the completion is
    /// dropped: replaying it would mark terminal-success over that verdict, and
    /// the build state machine would then refuse every correction.
    #[tokio::test]
    async fn settle_reports_a_commit_that_did_not_land() {
        let tracker = Arc::new(CommitTracker::default());
        let guard = tracker.start("build:1");
        guard.fail();
        drop(guard);

        assert!(!tracker.settle("build:1").await);
        assert!(
            tracker.settle("build:1").await,
            "the verdict is consumed with the job, so a later job id reads clean"
        );
    }

    /// A job that pushed nothing completes without waiting for anything.
    #[tokio::test]
    async fn settle_is_immediate_when_no_commit_is_owed() {
        let tracker = Arc::new(CommitTracker::default());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), tracker.settle("build:1"))
                .await
                .expect("settle blocked on a job that owes nothing")
        );
    }
}

#[cfg(test)]
mod nar_receive_store_tests {
    use super::{AppendOutcome, ENDED_STREAM_GRACE, MAX_ACTIVE_STREAMS, NarReceiveStore};
    use crate::messages::ClientMessage;
    use crate::session::frame::{Frame, Inbound, WireMessage as _};
    use gradient_util::shutdown::Shutdown;
    use sha2::Digest as _;
    use std::time::Duration;
    use tempfile::TempDir;

    fn assert_ok(o: AppendOutcome) {
        assert!(matches!(o, AppendOutcome::Ok), "expected Ok");
    }

    fn poisoned(s: &NarReceiveStore, job: &str, store_path: &str) -> bool {
        s.poison_reason(job, store_path).is_some()
    }

    fn store_with(
        max_bytes: u64,
        max_streams: usize,
        ended_grace: Duration,
    ) -> (TempDir, NarReceiveStore) {
        let dir = TempDir::new().unwrap();
        let mut s = NarReceiveStore::new(
            dir.path().to_path_buf(),
            "peer-1",
            Duration::from_secs(3600),
            max_bytes,
            0,
            Shutdown::new(),
        )
        .unwrap();
        // The idle backstop is off unless a test asks for it, so a test that
        // means to exercise the mark is never released by the other rule.
        s.set_limits(max_streams, ended_grace, Duration::ZERO);
        (dir, s)
    }

    fn store(max_bytes: u64) -> (TempDir, NarReceiveStore) {
        store_with(max_bytes, MAX_ACTIVE_STREAMS, ENDED_STREAM_GRACE)
    }

    /// A `NarPush` as it reaches the handler: encoded, then read back in place.
    fn frame(
        job: &str,
        path: &str,
        offset: u64,
        data: &[u8],
        is_final: bool,
    ) -> Frame<ClientMessage> {
        let msg = ClientMessage::NarPush {
            job_id: job.into(),
            store_path: path.into(),
            data: data.to_vec(),
            offset,
            is_final,
        };
        match ClientMessage::decode(msg.encode().expect("encodes")).expect("decodes") {
            Inbound::Bulk(f) => f,
            Inbound::Control(_) => panic!("NarPush is bulk"),
        }
    }

    /// A valid 32-char-hash store path keyed by a single repeated char.
    fn path(c: char) -> String {
        format!("/nix/store/{}-name", c.to_string().repeat(32))
    }

    /// A valid store path per index, for opening many streams at once.
    fn numbered_path(i: usize) -> String {
        format!("/nix/store/{i:032}-name")
    }

    const JOB: &str = "build:job-1";

    #[tokio::test]
    async fn chunks_are_staged_off_the_caller_and_finish_reports_the_hash() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 256], false))
                .await,
        );
        assert_ok(
            s.append(JOB, &a, 256, frame(JOB, &a, 256, &[1u8; 256], true))
                .await,
        );
        let staged = s.take_staged(JOB, &a).await.expect("stream is active");
        assert!(
            s.take_staged(JOB, &a).await.is_none(),
            "take_staged must detach the stream"
        );

        let file = staged.finish().await.expect("drained");
        assert_eq!(file.len, 512);
        let mut want = vec![0u8; 256];
        want.extend([1u8; 256]);
        assert_eq!(file.sha256, <[u8; 32]>::from(sha2::Sha256::digest(&want)));
        assert_eq!(tokio::fs::read(&file.path).await.unwrap(), want);
    }

    #[tokio::test]
    async fn non_contiguous_offset_poisons_path() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 100], false))
                .await,
        );
        assert!(matches!(
            s.append(JOB, &a, 999, frame(JOB, &a, 999, &[0u8; 10], true))
                .await,
            AppendOutcome::Overflow
        ));
        assert!(poisoned(&s, JOB, &a));
        assert!(matches!(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 10], true))
                .await,
            AppendOutcome::Poisoned
        ));
    }

    /// The stager re-checks the offset the frame itself carries, so a gap the
    /// session actor could not see still fails the commit instead of silently
    /// storing a short NAR.
    #[tokio::test]
    async fn a_frame_whose_offset_skips_ahead_fails_the_finish() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 100], false))
                .await,
        );
        assert_ok(
            s.append(JOB, &a, 100, frame(JOB, &a, 999, &[0u8; 10], true))
                .await,
        );
        let staged = s.take_staged(JOB, &a).await.expect("stream is active");
        assert!(staged.finish().await.is_err(), "the stager saw the gap");
    }

    #[tokio::test]
    async fn append_overflow_poisons_path() {
        let (_d, mut s) = store(300);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 200], false))
                .await,
        );
        assert!(matches!(
            s.append(JOB, &a, 200, frame(JOB, &a, 200, &[0u8; 200], false))
                .await,
            AppendOutcome::Overflow
        ));
        assert!(poisoned(&s, JOB, &a));
        assert!(matches!(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 10], true))
                .await,
            AppendOutcome::Poisoned
        ));
    }

    #[tokio::test]
    async fn overflow_across_keys_is_caught() {
        let (_d, mut s) = store(800);
        let (a, b, c) = (path('a'), path('b'), path('c'));
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 400], true))
                .await,
        );
        assert_ok(
            s.append(JOB, &b, 0, frame(JOB, &b, 0, &[0u8; 400], true))
                .await,
        );
        assert!(matches!(
            s.append(JOB, &c, 0, frame(JOB, &c, 0, &[42u8], true)).await,
            AppendOutcome::Overflow
        ));
        assert!(poisoned(&s, JOB, &c));
    }

    #[tokio::test]
    async fn note_header_reports_resumable_prefix() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        s.note_header(JOB, &a, "tok-v1").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, b"hello", false))
                .await,
        );
        // Simulated reconnect: same token resumes; a different token restarts.
        assert_eq!(s.note_header(JOB, &a, "tok-v1").await, 5);
        assert_eq!(s.note_header(JOB, &a, "tok-v2").await, 0);
    }

    /// Two jobs on one worker pushing the SAME store path concurrently must not
    /// share staging state: job B's mismatched-token header must not discard
    /// job A's in-flight partial, or job A's next chunk lands non-contiguous and
    /// a valid transfer is poisoned (the #502 cache-test stall).
    #[tokio::test]
    async fn concurrent_jobs_same_path_do_not_collide() {
        let (_d, mut s) = store(1_000_000);
        let p = path('a');

        s.note_header("build:job-a", &p, "tok-a").await;
        assert_ok(
            s.append(
                "build:job-a",
                &p,
                0,
                frame("build:job-a", &p, 0, &[0u8; 100], false),
            )
            .await,
        );

        // Job B opens the same path with a different token, then writes its own
        // first chunk - this must not touch job A's partial.
        s.note_header("build:job-b", &p, "tok-b").await;
        assert_ok(
            s.append(
                "build:job-b",
                &p,
                0,
                frame("build:job-b", &p, 0, &[1u8; 100], true),
            )
            .await,
        );

        // Job A resumes contiguously from its own 100 bytes.
        assert_ok(
            s.append(
                "build:job-a",
                &p,
                100,
                frame("build:job-a", &p, 100, &[0u8; 100], true),
            )
            .await,
        );
        assert!(!poisoned(&s, "build:job-a", &p));

        let sa = s
            .take_staged("build:job-a", &p)
            .await
            .expect("job-a staged");
        let sb = s
            .take_staged("build:job-b", &p)
            .await
            .expect("job-b staged");
        assert_eq!(sa.finish().await.expect("job-a drained").len, 200);
        assert_eq!(sb.finish().await.expect("job-b drained").len, 100);
    }

    /// A peer that opens streams and never pushes a byte is invisible to the
    /// byte budget, so the stream count is what stops it taking the process's
    /// file descriptors down with it.
    #[tokio::test]
    async fn a_header_flood_is_capped() {
        const CAP: usize = 4;
        let (_d, mut s) = store_with(10_000_000, CAP, ENDED_STREAM_GRACE);
        for i in 0..CAP {
            let p = numbered_path(i);
            s.note_header(JOB, &p, "tok").await;
            assert!(!poisoned(&s, JOB, &p), "stream {i} is within the cap");
        }

        let over = numbered_path(CAP);
        assert_eq!(s.note_header(JOB, &over, "tok").await, 0);
        assert!(
            poisoned(&s, JOB, &over),
            "the cap must poison rather than open another descriptor"
        );

        // Re-heading a stream that is already open is not one over the ceiling.
        let open = numbered_path(0);
        s.note_header(JOB, &open, "tok").await;
        assert!(!poisoned(&s, JOB, &open));

        // Ending the job frees the slots again: the sweep in note_header runs
        // before the cap test, so the next header is accepted.
        s.set_limits(CAP, Duration::ZERO, Duration::ZERO);
        s.forget_job(JOB).await;
        let next = numbered_path(CAP + 1);
        s.note_header(JOB, &next, "tok").await;
        assert!(!poisoned(&s, JOB, &next), "a swept slot must be reusable");
    }

    /// The frames that follow a `JobFailed` are the abandoned streams' own
    /// queued chunks, flushing behind the control frame that overtook them, so
    /// a chunk must extend the grace rather than cancel it.
    #[tokio::test]
    async fn a_trailing_chunk_does_not_revive_an_abandoned_stream() {
        let hour = Duration::from_secs(3600);
        let (_d, mut s) = store_with(1_000_000, MAX_ACTIVE_STREAMS, hour);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 64], false))
                .await,
        );

        s.forget_job(JOB).await;
        assert_ok(
            s.append(JOB, &a, 64, frame(JOB, &a, 64, &[0u8; 64], false))
                .await,
        );

        s.set_limits(MAX_ACTIVE_STREAMS, Duration::ZERO, Duration::ZERO);
        s.sweep_stale().await;
        assert!(
            s.take_staged(JOB, &a).await.is_none(),
            "the trailing chunk must not have cleared the mark"
        );
        assert_eq!(
            s.note_header(JOB, &a, "tok").await,
            0,
            "the abandoned partial must be gone from disk"
        );
    }

    /// Nothing marks a stream whose job task died without a terminal frame, or
    /// one opened by a header that arrived after its job ended, so silence
    /// alone has to release it.
    #[tokio::test]
    async fn an_idle_stream_is_released_without_any_mark() {
        let hour = Duration::from_secs(3600);
        let (_d, mut s) = store_with(1_000_000, MAX_ACTIVE_STREAMS, hour);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 64], false))
                .await,
        );

        // Still open: a swept stream would have restarted at 0 and rejected
        // this chunk as non-contiguous.
        s.sweep_stale().await;
        assert_ok(
            s.append(JOB, &a, 64, frame(JOB, &a, 64, &[0u8; 8], false))
                .await,
        );

        s.set_limits(MAX_ACTIVE_STREAMS, hour, Duration::from_nanos(1));
        s.sweep_stale().await;
        assert!(
            s.take_staged(JOB, &a).await.is_none(),
            "a stream idle past the partial TTL is released even unmarked"
        );
    }

    /// A `NarUploaded` that its `JobFailed` overtook must still find the reason
    /// its stream was rejected for, rather than being committed as a presigned
    /// upload that was never PUT.
    #[tokio::test]
    async fn a_poison_outlives_its_job_for_the_grace() {
        let (_d, mut s) = store(100);
        let a = path('a');
        assert!(matches!(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 200], false))
                .await,
            AppendOutcome::Overflow
        ));

        s.forget_job(JOB).await;
        assert!(poisoned(&s, JOB, &a), "the reason survives the job ending");

        s.set_limits(MAX_ACTIVE_STREAMS, Duration::ZERO, Duration::ZERO);
        s.sweep_stale().await;
        assert!(!poisoned(&s, JOB, &a), "and is swept with the streams");
    }

    /// A worker drops the uploads still in flight when a job ends, so the
    /// streams they opened are only ever released by the job ending.
    #[tokio::test]
    async fn forget_job_releases_the_streams_a_job_abandoned() {
        let (_d, mut s) = store_with(1_000_000, MAX_ACTIVE_STREAMS, Duration::ZERO);
        let (a1, a2, b) = (path('a'), path('b'), path('c'));
        for p in [&a1, &a2] {
            s.note_header("build:job-a", p, "tok").await;
            assert_ok(
                s.append(
                    "build:job-a",
                    p,
                    0,
                    frame("build:job-a", p, 0, &[0u8; 64], false),
                )
                .await,
            );
        }
        s.note_header("build:job-b", &b, "tok").await;
        assert_ok(
            s.append(
                "build:job-b",
                &b,
                0,
                frame("build:job-b", &b, 0, &[7u8; 64], true),
            )
            .await,
        );

        s.forget_job("build:job-a").await;

        let staged = s
            .take_staged("build:job-b", &b)
            .await
            .expect("another job's stream is untouched");
        assert_eq!(staged.finish().await.expect("drained").len, 64);

        assert!(s.take_staged("build:job-a", &a1).await.is_none());
        assert!(s.take_staged("build:job-a", &a2).await.is_none());
        assert_eq!(
            s.note_header("build:job-a", &a1, "tok").await,
            0,
            "the abandoned partial must be gone from disk"
        );
    }

    /// `JobCompleted` rides the control lane and overtakes the job's own
    /// trailing `NarUploaded` on the bulk lane, so a stream whose job has just
    /// ended must still commit.
    #[tokio::test]
    async fn a_trailing_nar_uploaded_survives_the_job_ending() {
        let (_d, mut s) = store(1_000_000);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[1u8; 128], true))
                .await,
        );

        s.forget_job(JOB).await;

        let staged = s
            .take_staged(JOB, &a)
            .await
            .expect("the stream outlives its job for the grace");
        assert_eq!(staged.finish().await.expect("drained").len, 128);
    }

    /// A staging open that fails (here: a directory sitting where the
    /// `.partial` must go) has to poison with its own error, so the header can
    /// abort the job instead of inviting the whole NAR and blaming the budget.
    #[tokio::test]
    async fn a_staging_open_failure_poisons_with_its_own_reason() {
        let dir = TempDir::new().unwrap();
        let blocked = dir
            .path()
            .join(format!("peer-1/{JOB}/{}.partial", "a".repeat(32)));
        tokio::fs::create_dir_all(&blocked).await.unwrap();

        let mut s = NarReceiveStore::new(
            dir.path().to_path_buf(),
            "peer-1",
            Duration::from_secs(3600),
            1024,
            0,
            Shutdown::new(),
        )
        .unwrap();

        let a = path('a');
        assert_eq!(s.note_header(JOB, &a, "tok").await, 0);
        assert!(poisoned(&s, JOB, &a));
        let reason = s.poison_reason(JOB, &a).expect("a reason is recorded");
        assert!(
            reason.contains("open the staged partial"),
            "the reason must name the staging failure, got: {reason}"
        );
    }

    #[tokio::test]
    async fn a_relayed_nar_lands_in_the_object_store_on_the_local_backend() {
        let dir = TempDir::new().unwrap();
        let store = gradient_storage::NarStore::local(dir.path().to_str().unwrap()).unwrap();
        let claim = dir.path().join("claim");
        tokio::fs::write(&claim, b"nar").await.unwrap();

        let confirmed = super::place_relayed(&store, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &claim)
            .await
            .unwrap();

        assert!(confirmed);
        assert!(
            store
                .exists("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .await
                .unwrap()
        );
        assert!(!claim.exists());
    }

    #[tokio::test]
    async fn a_relayed_nar_is_staged_unconfirmed_on_an_s3_backend() {
        let dir = TempDir::new().unwrap();
        let store = gradient_storage::NarStore::s3(
            "bucket",
            "us-east-1",
            Some("http://127.0.0.1:1"),
            Some("key"),
            Some("secret"),
            "",
            false,
            gradient_storage::S3Timeouts::default(),
        )
        .unwrap()
        .with_staging(gradient_storage::StagedNars::new(dir.path().join("nar-staged")).unwrap());
        let claim = dir.path().join("claim");
        tokio::fs::write(&claim, b"nar").await.unwrap();

        let confirmed = super::place_relayed(&store, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &claim)
            .await
            .unwrap();

        assert!(!confirmed, "the object is owed to S3");
        assert!(
            store
                .staged()
                .unwrap()
                .exists("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .await
        );
        assert!(!claim.exists());
    }

    /// A push stream is append-only once open: a sender that restarts at 0
    /// mid-stream is rejected here rather than silently truncating a partial
    /// the session still believes in.
    #[tokio::test]
    async fn a_restart_at_offset_zero_is_fatal_for_an_open_stream() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        s.note_header(JOB, &a, "tok").await;
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 100], false))
                .await,
        );
        assert!(matches!(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 100], false))
                .await,
            AppendOutcome::Overflow
        ));
        assert!(poisoned(&s, JOB, &a));
    }

    #[tokio::test]
    async fn presigned_mode_has_no_active_stream() {
        let (_d, mut s) = store(1024);
        assert!(
            s.take_staged(JOB, &path('a')).await.is_none(),
            "a path with no header/push must not be treated as direct mode"
        );
    }

    #[tokio::test]
    async fn clear_poison_allows_retry() {
        let (_d, mut s) = store(100);
        let a = path('a');
        assert!(matches!(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 200], false))
                .await,
            AppendOutcome::Overflow
        ));
        assert!(poisoned(&s, JOB, &a));
        s.clear_poison(JOB, &a).await;
        assert!(!poisoned(&s, JOB, &a));
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, &[0u8; 50], true))
                .await,
        );
    }

    #[tokio::test]
    async fn finish_discards_staged_partial() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        assert_ok(
            s.append(JOB, &a, 0, frame(JOB, &a, 0, b"hello", true))
                .await,
        );
        s.finish(JOB, &a).await;
        assert!(s.take_staged(JOB, &a).await.is_none());
        assert_eq!(
            s.note_header(JOB, &a, "").await,
            0,
            "partial must be gone from disk"
        );
    }
}

#[cfg(test)]
mod serve_nar_tests {
    use super::*;
    use crate::session::frame::WireMessage;
    use bytes::Bytes;
    use gradient_test_support::state::test_state;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tokio::sync::mpsc;

    /// Spy writer: records every message the server attempted to send so the
    /// test can assert exactly which protocol frames were emitted (NarPush,
    /// NarUnavailable, NarAbort, …).
    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel::<Bytes>(64);
        (
            ProtoWriter {
                control_tx: tx.clone(),
                tx,
                send_chunk_timeout: timeout,
                _direction: std::marker::PhantomData,
            },
            rx,
        )
    }

    fn decode(bytes: Bytes) -> ServerMessage {
        ServerMessage::decode(bytes)
            .expect("decode ServerMessage")
            .into_message()
            .expect("deserialise ServerMessage")
    }

    /// Streamed payload arrives as one or more `NarPush` frames whose
    /// concatenated `data` matches the original bytes, with the final frame
    /// flagged `is_final=true`.
    #[tokio::test]
    async fn serve_streams_full_payload_in_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let mut payload = Vec::with_capacity(9 * 1024 * 1024);
        for i in 0..(9 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state.nar_storage.put(hash, payload.clone()).await.unwrap();

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(&state, &writer, "job-1", &store_path, 0, None)
            .await
            .expect("serve must succeed");

        let mut assembled = Vec::with_capacity(payload.len());
        let mut nar_push_frames = 0u32;
        let mut saw_header = false;
        let mut saw_final = false;
        while let Ok(bytes) = rx.try_recv() {
            match decode(bytes) {
                ServerMessage::NarStreamHeader { total_bytes, .. } => {
                    saw_header = true;
                    assert!(!saw_final, "header must precede chunks");
                    assert_eq!(total_bytes as usize, payload.len());
                }
                ServerMessage::NarPush { data, is_final, .. } => {
                    assembled.extend_from_slice(&data);
                    if is_final {
                        saw_final = true;
                    }
                    nar_push_frames += 1;
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            }
        }
        assert!(saw_header, "a NarStreamHeader must precede the chunks");
        assert!(
            nar_push_frames >= 3,
            "9 MiB in 512 KiB chunks is at least 3 frames, got {nar_push_frames}"
        );
        assert!(saw_final, "the last frame must be is_final=true");
        assert_eq!(
            assembled, payload,
            "concatenated NarPush data must equal source"
        );
    }

    /// Missing object → `NarUnavailable` (not `NarAbort`, no NarPush) and an
    /// `Err` from `serve_nar_request`.
    #[tokio::test]
    async fn serve_emits_nar_unavailable_when_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &state,
            &writer,
            "job-1",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-missing",
            0,
            None,
        )
        .await;
        assert!(res.is_err(), "missing path must surface as Err");

        let bytes = rx.try_recv().expect("expect one frame");
        let msg = decode(bytes);
        assert_eq!(msg.variant_name(), "NarUnavailable");
        assert!(
            rx.try_recv().is_err(),
            "no further frames after NarUnavailable"
        );
    }

    /// RAM answers a request the object store could not: the hash is only in
    /// the hot cache.
    #[tokio::test]
    async fn serve_answers_a_hot_entry_in_bulk_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cli = gradient_test_support::prelude::test_cli();
        let storage = gradient_storage::NarStore::local(&cli.storage.base_path)
            .unwrap()
            .with_hot_cache(gradient_storage::HotNarCache::new(
                4 * 1024 * 1024,
                2 * 1024 * 1024,
            ));
        let state = gradient_test_support::prelude::test_state_with_storage(db, storage);
        let payload: Vec<u8> = (0..(1024 * 1024 + 123)).map(|i| i as u8).collect();
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state
            .nar_storage
            .hot()
            .insert(hash, Bytes::from(payload.clone()));

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(&state, &writer, "job-1", &store_path, 0, None)
            .await
            .expect("serve from RAM");

        let mut assembled = Vec::new();
        let mut frames = 0u32;
        while let Ok(bytes) = rx.try_recv() {
            match decode(bytes) {
                ServerMessage::NarStreamHeader { total_bytes, .. } => {
                    assert_eq!(total_bytes as usize, payload.len());
                }
                ServerMessage::NarPush { data, .. } => {
                    assembled.extend_from_slice(&data);
                    frames += 1;
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            }
        }
        assert_eq!(assembled, payload);
        assert!(
            frames >= 3,
            "1 MiB in 512 KiB chunks is at least three frames"
        );
        assert_eq!(state.nar_storage.hot().stats().hits, 1);
    }

    #[tokio::test]
    async fn serve_reads_a_staged_file_when_the_object_is_absent() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cli = gradient_test_support::prelude::test_cli();
        let staged_root = std::path::PathBuf::from(&cli.storage.base_path).join("nar-staged");
        let storage = gradient_storage::NarStore::local(&cli.storage.base_path)
            .unwrap()
            .with_staging(gradient_storage::StagedNars::new(&staged_root).unwrap());
        let state = gradient_test_support::prelude::test_state_with_storage(db, storage);
        let claim = std::path::PathBuf::from(&cli.storage.base_path).join("claim");
        tokio::fs::write(&claim, b"staged payload").await.unwrap();
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state
            .nar_storage
            .staged()
            .unwrap()
            .adopt(hash, &claim)
            .await
            .unwrap();

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        serve_nar_request(
            &state,
            &writer,
            "job-1",
            &format!("/nix/store/{hash}-pkg"),
            0,
            None,
        )
        .await
        .expect("serve from staging");

        let mut assembled = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            if let ServerMessage::NarPush { data, .. } = decode(bytes) {
                assembled.extend_from_slice(&data);
            }
        }
        assert_eq!(assembled, b"staged payload");
    }

    #[tokio::test]
    async fn a_young_unconfirmed_row_is_awaiting_upload_and_an_old_or_confirmed_one_is_not() {
        fn row(confirmed: bool, age: chrono::Duration) -> gradient_entity::cached_path::Model {
            gradient_entity::cached_path::Model {
                hash: "abcdefghijklmnopqrstuvwxyz012345".into(),
                file_hash: Some("sha256:x".into()),
                confirmed,
                created_at: gradient_types::now() - age,
                ..Default::default()
            }
        }

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![row(false, chrono::Duration::minutes(1))],
                vec![row(true, chrono::Duration::minutes(1))],
                vec![row(false, chrono::Duration::hours(48))],
            ])
            .into_connection();
        let state = test_state(db);
        let hash = "abcdefghijklmnopqrstuvwxyz012345";

        assert!(awaiting_upload(&state, hash).await, "young and unconfirmed");
        assert!(!awaiting_upload(&state, hash).await, "confirmed");
        assert!(!awaiting_upload(&state, hash).await, "older than the grace");
    }
}
