/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! NAR transfer: inbound push staging, outbound serving, and the
//! `DispatchContext` handlers that commit an upload once it is complete.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use tracing::{debug, error, warn};

use crate::messages::ServerMessage;

use super::dispatch::DispatchContext;
use super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::socket::{NAR_PUSH_CHUNK_SIZE, ProtoWriter, send_server_msg};

// ── Per-session inbound NAR receive store (issue #109, resumable #225) ────────

/// Outcome of [`NarReceiveStore::append`].
pub(super) enum AppendOutcome {
    /// Chunk was staged.
    Ok,
    /// Fatal: the chunk exceeded the session budget or arrived at a
    /// non-contiguous offset. The path is now poisoned and its partial
    /// discarded - the caller aborts the job and rejects the eventual
    /// `NarUploaded` for the same path.
    Overflow,
    /// Chunk arrived for a path the session has already poisoned. Drop it.
    Poisoned,
}

#[derive(Default)]
struct PathState {
    /// Sender's `stream_token`; empty for legacy pushes that skip the header.
    token: String,
    /// Bytes staged for this path on this session (resumed prefix + appends).
    staged: u64,
}

/// A staged direct-mode upload detached from the session's receive store so a
/// spawned task can validate, read, and commit it off the read loop.
pub(super) struct StagedNar {
    store: gradient_storage::PartialStore,
    key: String,
    token: String,
}

/// Disk-backed receiver for inbound `NarPush` chunks. Each push is staged to a
/// `*.partial` file under `<base_path>/nar-partial/<peer_id>/<job_id>/<hash>` so
/// an interrupted upload can resume from a byte offset (issue #225) and a large
/// NAR no longer pins RAM. A per-session byte budget plus a poison set preserve
/// the #109 protection against a rogue worker opening many un-finalized streams
/// (the budget now bounds staged **disk**, not RAM). Keying by `peer_id` isolates
/// workers; keying by `job_id` isolates two jobs on one worker that push the
/// *same* content-addressed path concurrently - without it their interleaved
/// appends to a shared hash-keyed partial trip the contiguity check and poison a
/// valid transfer (mirrors the worker-pull `{job_id}/{hash}` namespacing).
pub(super) struct NarReceiveStore {
    store: gradient_storage::PartialStore,
    peer_id: String,
    max_bytes: u64,
    active: HashMap<String, PathState>,
    poisoned: BTreeSet<String>,
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
    ) -> anyhow::Result<Self> {
        Ok(Self {
            store: gradient_storage::PartialStore::new(root, ttl)?,
            peer_id: peer_id.to_owned(),
            max_bytes,
            active: HashMap::new(),
            poisoned: BTreeSet::new(),
        })
    }

    fn key(&self, job_id: &str, hash: &str) -> String {
        format!("{}/{}/{}", self.peer_id, job_id, hash)
    }

    /// Record the push stream's token and return how many bytes are already
    /// staged for it (0 on token mismatch / nothing on disk). Clears any stale
    /// poison so a fresh attempt can proceed.
    pub(super) async fn note_header(&mut self, job_id: &str, store_path: &str, token: &str) -> u64 {
        let sk = state_key(job_id, store_path);
        self.poisoned.remove(&sk);
        let received = match store_hash(store_path) {
            Some(h) => {
                let (store, key, token) =
                    (self.store.clone(), self.key(job_id, h), token.to_owned());
                tokio::task::spawn_blocking(move || store.received_len(&key, &token).unwrap_or(0))
                    .await
                    .unwrap_or(0)
            }
            None => 0,
        };
        self.active.insert(
            sk,
            PathState {
                token: token.to_owned(),
                staged: received,
            },
        );
        received
    }

    /// Stage a chunk at `offset` (must be contiguous). Creates a token-less
    /// entry for legacy pushes that skip the header. The blocking disk write
    /// runs on the blocking pool so the async socket task is never stalled.
    pub(super) async fn append(
        &mut self,
        job_id: &str,
        store_path: &str,
        offset: u64,
        data: &[u8],
    ) -> AppendOutcome {
        let sk = state_key(job_id, store_path);
        if self.poisoned.contains(&sk) {
            return AppendOutcome::Poisoned;
        }

        let Some(hash) = store_hash(store_path) else {
            return AppendOutcome::Poisoned;
        };

        self.active.entry(sk.clone()).or_default();
        let total: u64 = self.active.values().map(|s| s.staged).sum();
        if total.saturating_add(data.len() as u64) > self.max_bytes {
            self.poison(job_id, store_path, hash).await;
            return AppendOutcome::Overflow;
        }

        let token = self.active[&sk].token.clone();
        let (store, key, data) = (self.store.clone(), self.key(job_id, hash), data.to_vec());
        let len = data.len() as u64;
        match tokio::task::spawn_blocking(move || store.append(&key, &token, offset, &data)).await {
            Ok(Ok(())) => {
                if let Some(s) = self.active.get_mut(&sk) {
                    s.staged += len;
                }
                AppendOutcome::Ok
            }
            Ok(Err(e)) => {
                warn!(%store_path, error = %e, "partial append failed; poisoning path");
                self.poison(job_id, store_path, hash).await;
                AppendOutcome::Overflow
            }
            Err(e) => {
                warn!(%store_path, error = %e, "partial append task panicked; poisoning path");
                self.poison(job_id, store_path, hash).await;
                AppendOutcome::Overflow
            }
        }
    }

    async fn poison(&mut self, job_id: &str, store_path: &str, hash: &str) {
        let (store, key) = (self.store.clone(), self.key(job_id, hash));
        let _ = tokio::task::spawn_blocking(move || store.discard(&key)).await;
        let sk = state_key(job_id, store_path);
        self.active.remove(&sk);
        self.poisoned.insert(sk);
    }

    /// Detach the staged stream for `store_path` so a spawned task can commit
    /// it without borrowing the session's receive store. Returns `None` when no
    /// direct-mode stream is open (a presigned upload). The per-path state is
    /// removed and the finished partial is *claimed* under a unique key here,
    /// synchronously on the read loop: the commit runs detached and can lag
    /// behind the commit semaphore, and the same content-addressed `.drv` is
    /// pushed repeatedly across an eval's closure walk. A later push of the same
    /// hash resets the shared `{peer}/{hash}` partial (token-mismatch discard /
    /// `offset==0` truncate), so a bare shared key would leave the queued commit
    /// reading 0 bytes ("staged NAR size 0 does not match reported file_size").
    pub(super) fn take_staged(&mut self, job_id: &str, store_path: &str) -> Option<StagedNar> {
        let state = self.active.remove(&state_key(job_id, store_path))?;
        let hash = store_hash(store_path)?;
        let base_key = self.key(job_id, hash);
        let key = match self.store.detach(&base_key) {
            Ok(Some(claim)) => claim,
            Ok(None) => base_key,
            Err(e) => {
                warn!(%store_path, error = %e, "failed to claim staged partial; using shared key");
                base_key
            }
        };
        Some(StagedNar {
            store: self.store.clone(),
            key,
            token: state.token,
        })
    }

    /// Drop the staged partial and per-path state after a successful commit.
    pub(super) async fn finish(&mut self, job_id: &str, store_path: &str) {
        self.active.remove(&state_key(job_id, store_path));
        if let Some(hash) = store_hash(store_path) {
            let (store, key) = (self.store.clone(), self.key(job_id, hash));
            let _ = tokio::task::spawn_blocking(move || store.discard(&key)).await;
        }
    }

    /// Has this path been poisoned by a prior overflow on the same session?
    pub(super) fn is_poisoned(&self, job_id: &str, store_path: &str) -> bool {
        self.poisoned.contains(&state_key(job_id, store_path))
    }

    /// Forget the poison flag and discard any partial for `store_path` so a
    /// later, well-formed retry of the same path can proceed.
    pub(super) async fn clear_poison(&mut self, job_id: &str, store_path: &str) {
        self.poisoned.remove(&state_key(job_id, store_path));
        self.finish(job_id, store_path).await;
    }

    pub(super) fn max_bytes(&self) -> u64 {
        self.max_bytes
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
        debug!(peer_id = %self.peer_id, %job_id, %store_path, received, "NarStreamHeader (push)");
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
        job_id: String,
        store_path: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
        nar: &mut NarReceiveStore,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");
        if data.is_empty() {
            return;
        }
        match nar.append(&job_id, &store_path, offset, &data).await {
            AppendOutcome::Ok => {}
            AppendOutcome::Overflow => {
                let reason = format!(
                    "NAR upload for {store_path} rejected: staged-partial budget ({} bytes) \
                     exceeded or non-contiguous offset {offset}",
                    nar.max_bytes(),
                );
                warn!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "poisoning NAR path");
                self.abort_job(&job_id, reason).await;
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
    #[allow(clippy::too_many_arguments)] // mirrors the wire-protocol message fields
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
        nar: &mut NarReceiveStore,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, %file_hash, file_size, nar_size, %nar_hash, ?deriver, "NarUploaded");

        // Reject any NarUploaded for a path whose chunked transfer was rejected
        // mid-stream. Without this guard `mark_nar_stored` would record a
        // `cached_path` row whose bytes never reached `nar_storage` - leaving
        // the path "cached" in the DB and undeliverable on the next download.
        if nar.is_poisoned(&job_id, &store_path) {
            nar.clear_poison(&job_id, &store_path).await;
            let reason = format!(
                "NarUploaded for {store_path} rejected: prior NarPush chunk \
                 exceeded the staged-partial budget or arrived out of order"
            );
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

        // Resolve the owning org here, on the read loop, while the job is still
        // active. The detached commit runs after this method returns, and the
        // very next message on the connection (`JobComplete`) evicts the job
        // from the tracker - so re-resolving `org_for_job` inside the task would
        // race to `None`, drop the `cached_path_signature` placeholder, and the
        // narinfo would 404 forever with the path stuck unsigned.
        let org_id = self.scheduler.org_for_job(&job_id).await;

        // The commit reads the whole staged NAR and writes it to `nar_storage`
        // (an S3 upload on object-store backends). Inline it froze this
        // session's read loop for the duration, so the worker's own bounded
        // sends backed up, the worker stopped reading, and every concurrent
        // transfer on the connection stalled into its send timeout
        // ("WebSocket send stalled on final NarPush"). Detach the staged
        // stream synchronously, then commit on a bounded spawned task.
        let staged = nar.take_staged(&job_id, &store_path);
        let writer = self.writer.clone();
        let state = Arc::clone(self.state);
        let scheduler = Arc::clone(self.scheduler);
        let peer_id = self.peer_id.to_owned();
        let semaphore = Arc::clone(self.nar_commit_semaphore);
        let hash = hash.to_owned();
        tokio::spawn(async move {
            let Ok(_permit) = semaphore.acquire_owned().await else {
                return;
            };
            commit_uploaded_nar(CommitUploadedNar {
                writer,
                state,
                scheduler,
                peer_id,
                job_id,
                org_id,
                store_path,
                hash,
                file_hash,
                file_size,
                nar_size,
                nar_hash,
                references,
                deriver,
                staged,
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
    org_id: Option<gradient_types::ids::OrganizationId>,
    store_path: String,
    hash: String,
    file_hash: String,
    file_size: u64,
    nar_size: u64,
    nar_hash: String,
    references: Vec<String>,
    deriver: Option<String>,
    staged: Option<StagedNar>,
}

/// Detached storage commit plus DB effects for one `NarUploaded`.
async fn commit_uploaded_nar(c: CommitUploadedNar) {
    let committed = match c.staged {
        Some(ref staged) => {
            commit_relayed(
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
        }
        None => {
            commit_presigned(
                &c.writer,
                &c.state,
                &c.scheduler,
                &c.peer_id,
                &c.job_id,
                &c.store_path,
                &c.hash,
                c.file_size,
            )
            .await
        }
    };
    if !committed {
        return;
    }

    let file_size_i64 = c.file_size as i64;
    let nar_record = NarUploadRecord {
        file_hash: &c.file_hash,
        file_size: file_size_i64,
        nar_size: c.nar_size as i64,
        nar_hash: &c.nar_hash,
        references: &c.references,
        deriver: c.deriver.as_deref(),
    };
    if let Err(e) = mark_nar_stored(&c.state, c.org_id, &c.store_path, &nar_record).await {
        warn!(store_path = %c.store_path, error = %e, "failed to mark NAR as stored");
    }
    if let Err(e) = record_nar_push_metric(&c.state, c.org_id, file_size_i64).await {
        debug!(error = %e, "failed to record cache metric for NarUploaded");
    }
}

/// Commit a direct-mode (relayed) push: validate the staged partial's size
/// against the reported `file_size`, write it to `nar_storage`, then drop
/// the partial. Returns `false` (after failing the build transiently) if
/// any step fails.
#[allow(clippy::too_many_arguments)]
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
    staged: &StagedNar,
) -> bool {
    let (store, key, token) = (
        staged.store.clone(),
        staged.key.clone(),
        staged.token.clone(),
    );
    let staged_len =
        tokio::task::spawn_blocking(move || store.received_len(&key, &token).unwrap_or(0))
            .await
            .unwrap_or(0);
    if staged_len != file_size {
        let reason =
            format!("staged NAR size {staged_len} does not match reported file_size {file_size}");
        error!(%peer_id, %job_id, %store_path, %reason, "NAR upload integrity check failed");
        fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
        return false;
    }
    let (store, key) = (staged.store.clone(), staged.key.clone());
    let buf = match tokio::task::spawn_blocking(move || store.read_all(&key)).await {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            let reason = format!("failed to read staged NAR: {e}");
            error!(%peer_id, %job_id, %store_path, error = %e, "read staged NAR failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            return false;
        }
        Err(e) => {
            let reason = format!("read staged NAR task panicked: {e}");
            error!(%peer_id, %job_id, %store_path, %reason, "read staged NAR failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            return false;
        }
    };
    if let Err(e) = crate::ingest::put_nar_idempotent(
        &state.worker_db,
        &state.nar_storage,
        hash,
        file_hash,
        buf,
    )
    .await
    {
        let reason = format!("failed to write NAR to storage: {e}");
        error!(%peer_id, %job_id, %store_path, error = %e, "nar_storage.put failed");
        fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
        return false;
    }
    let (store, key) = (staged.store.clone(), staged.key.clone());
    let _ = tokio::task::spawn_blocking(move || store.discard(&key)).await;
    debug!(%peer_id, %job_id, %store_path, file_size, "NAR stored");
    true
}

/// Commit a presigned (S3) upload: the worker already PUT the bytes
/// directly, so HEAD the object and compare its size against the reported
/// `file_size`. Returns `false` (after failing the build transiently) if
/// the object is missing or its size mismatches.
#[allow(clippy::too_many_arguments)]
async fn commit_presigned(
    writer: &ProtoWriter,
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    peer_id: &str,
    job_id: &str,
    store_path: &str,
    hash: &str,
    file_size: u64,
) -> bool {
    match state.nar_storage.head_size(hash).await {
        Ok(Some(size)) if size == file_size => true,
        Ok(Some(size)) => {
            let reason = format!(
                "presigned NAR object size {size} does not match reported file_size {file_size}"
            );
            error!(%peer_id, %job_id, %store_path, %reason, "presigned NAR upload integrity check failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            false
        }
        Ok(None) => {
            let reason = format!("presigned NAR object for {store_path} is missing from storage");
            error!(%peer_id, %job_id, %store_path, %reason, "presigned NAR upload integrity check failed");
            fail_build_transient(writer, scheduler, peer_id, job_id, reason).await;
            false
        }
        Err(e) => {
            let reason = format!("failed to head presigned NAR object: {e}");
            error!(%peer_id, %job_id, %store_path, error = %e, "presigned NAR HEAD failed");
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
///   ever held in memory. Chunks are coalesced/split to `NAR_PUSH_CHUNK_SIZE`.
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
        tokio::time::timeout(
            storage_open_timeout,
            state.nar_storage.get_stream_from(hash, offset),
        )
        .await
    };

    let (size, mut stream) = match open(resume_from).await {
        Ok(Ok(Some((size, s)))) => (size, s),
        Ok(Ok(None)) => {
            invalidate_cached_path(state, hash, store_path).await;
            let reason = format!("NAR not found in cache for {store_path}");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
        Ok(Err(e)) => {
            let reason = format!("nar_storage.get_stream({hash}) failed: {e}");
            error!(%store_path, error = %e, "NAR storage read error");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
        Err(_) => {
            let reason = format!(
                "nar_storage.get_stream({hash}) timed out after {}s",
                storage_open_timeout.as_secs()
            );
            warn!(%store_path, "NAR storage open timed out");
            return Err(
                fail_transfer(writer, job_id, store_path, FailKind::Unavailable, reason).await,
            );
        }
    };

    // The stored `.nar.zst` is immutable per hash, so the pull token is just
    // its size. A worker resuming with a stale token (or claiming more bytes
    // than exist) restarts from 0; the `NarStreamHeader.total_bytes` lets the
    // worker truncate its `.partial` accordingly.
    let server_token = format!("len-{size}");
    let token_mismatch = client_token.is_some_and(|t| t != server_token);
    let mut start = resume_from;
    if resume_from > size || token_mismatch {
        match open(0).await {
            Ok(Ok(Some((_s, s)))) => {
                stream = s;
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

    let mut buf: Vec<u8> = Vec::with_capacity(NAR_PUSH_CHUNK_SIZE);
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
            let want = NAR_PUSH_CHUNK_SIZE - buf.len();
            let take = slice.len().min(want);
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == NAR_PUSH_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(NAR_PUSH_CHUNK_SIZE));
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

    debug!(%store_path, bytes = total, chunks = chunks_sent, "NarRequest served (streaming)");
    Ok(())
}

/// Purge a `cached_path` row whose NAR is no longer in `nar_storage`.
///
/// Deletes the stale artifact and clears `derivation_output.is_cached` /
/// `cached_path` so the next `CacheQuery` stops claiming the path is available -
/// letting the next build either rebuild from source or pick the path up from a
/// configured upstream. The derivation graph is untouched.
async fn invalidate_cached_path(state: &Arc<ServerState>, hash: &str, store_path: &str) {
    match gradient_db::demote_cached_output(&state.worker_db, &state.nar_storage, hash).await {
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

#[cfg(test)]
mod nar_receive_store_tests {
    use super::{AppendOutcome, NarReceiveStore};
    use std::time::Duration;
    use tempfile::TempDir;

    fn assert_ok(o: AppendOutcome) {
        assert!(matches!(o, AppendOutcome::Ok), "expected Ok");
    }

    fn store(max_bytes: u64) -> (TempDir, NarReceiveStore) {
        let dir = TempDir::new().unwrap();
        let s = NarReceiveStore::new(
            dir.path().to_path_buf(),
            "peer-1",
            Duration::from_secs(3600),
            max_bytes,
        )
        .unwrap();
        (dir, s)
    }

    /// A valid 32-char-hash store path keyed by a single repeated char.
    fn path(c: char) -> String {
        format!("/nix/store/{}-name", c.to_string().repeat(32))
    }

    const JOB: &str = "build:job-1";

    #[tokio::test]
    async fn append_below_budget_stages_and_reads_back() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        assert_ok(s.append(JOB, &a, 0, &[0u8; 256]).await);
        assert_ok(s.append(JOB, &a, 256, &[1u8; 256]).await);
        let staged = s.take_staged(JOB, &a).expect("direct stream is active");
        assert!(
            s.take_staged(JOB, &a).is_none(),
            "take_staged must detach the stream"
        );
        assert_eq!(
            staged
                .store
                .received_len(&staged.key, &staged.token)
                .unwrap(),
            512
        );
        assert_eq!(staged.store.read_all(&staged.key).unwrap().len(), 512);
    }

    #[tokio::test]
    async fn non_contiguous_offset_poisons_path() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        assert_ok(s.append(JOB, &a, 0, &[0u8; 100]).await);
        assert!(matches!(
            s.append(JOB, &a, 999, &[0u8; 10]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(JOB, &a));
        assert!(matches!(
            s.append(JOB, &a, 0, &[0u8; 10]).await,
            AppendOutcome::Poisoned
        ));
    }

    #[tokio::test]
    async fn append_overflow_poisons_path() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        assert_ok(s.append(JOB, &a, 0, &[0u8; 1000]).await);
        assert!(matches!(
            s.append(JOB, &a, 1000, &[0u8; 100]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(JOB, &a));
        assert!(matches!(
            s.append(JOB, &a, 0, &[0u8; 50]).await,
            AppendOutcome::Poisoned
        ));
    }

    #[tokio::test]
    async fn overflow_across_keys_is_caught() {
        let (_d, mut s) = store(800);
        assert_ok(s.append(JOB, &path('a'), 0, &[0u8; 400]).await);
        assert_ok(s.append(JOB, &path('b'), 0, &[0u8; 400]).await);
        assert!(matches!(
            s.append(JOB, &path('c'), 0, &[42u8]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(JOB, &path('c')));
    }

    #[tokio::test]
    async fn note_header_reports_resumable_prefix() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        s.note_header(JOB, &a, "tok-v1").await;
        assert_ok(s.append(JOB, &a, 0, b"hello").await);
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
        assert_ok(s.append("build:job-a", &p, 0, &[0u8; 100]).await);

        // Job B opens the same path with a different token, then writes its own
        // first chunk - this must not touch job A's partial.
        s.note_header("build:job-b", &p, "tok-b").await;
        assert_ok(s.append("build:job-b", &p, 0, &[1u8; 100]).await);

        // Job A resumes contiguously from its own 100 bytes.
        assert_ok(s.append("build:job-a", &p, 100, &[0u8; 100]).await);
        assert!(!s.is_poisoned("build:job-a", &p));

        let sa = s.take_staged("build:job-a", &p).expect("job-a staged");
        let sb = s.take_staged("build:job-b", &p).expect("job-b staged");
        assert_eq!(sa.store.received_len(&sa.key, &sa.token).unwrap(), 200);
        assert_eq!(sb.store.received_len(&sb.key, &sb.token).unwrap(), 100);
    }

    #[test]
    fn presigned_mode_has_no_active_stream() {
        let (_d, mut s) = store(1024);
        assert!(
            s.take_staged(JOB, &path('a')).is_none(),
            "a path with no header/push must not be treated as direct mode"
        );
    }

    #[tokio::test]
    async fn clear_poison_allows_retry() {
        let (_d, mut s) = store(100);
        let a = path('a');
        assert!(matches!(
            s.append(JOB, &a, 0, &[0u8; 200]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(JOB, &a));
        s.clear_poison(JOB, &a).await;
        assert!(!s.is_poisoned(JOB, &a));
        assert_ok(s.append(JOB, &a, 0, &[0u8; 50]).await);
    }

    #[tokio::test]
    async fn finish_discards_staged_partial() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        assert_ok(s.append(JOB, &a, 0, b"hello").await);
        s.finish(JOB, &a).await;
        assert!(s.take_staged(JOB, &a).is_none());
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
    use crate::messages::decode_server_message;
    use gradient_test_support::state::test_state;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tokio::sync::mpsc;

    /// Spy writer: records every message the server attempted to send so the
    /// test can assert exactly which protocol frames were emitted (NarPush,
    /// NarUnavailable, NarAbort, …).
    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        (
            ProtoWriter {
                tx,
                send_chunk_timeout: timeout,
                _direction: std::marker::PhantomData,
            },
            rx,
        )
    }

    fn decode(bytes: &[u8]) -> ServerMessage {
        decode_server_message(bytes).expect("decode ServerMessage")
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
            match decode(&bytes) {
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
            "9 MiB / 4 MiB chunks → at least 3 frames, got {nar_push_frames}"
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
        let msg = decode(&bytes);
        assert_eq!(msg.variant_name(), "NarUnavailable");
        assert!(
            rx.try_recv().is_err(),
            "no further frames after NarUnavailable"
        );
    }
}
