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

/// Disk-backed receiver for inbound `NarPush` chunks. Each push is staged to a
/// `*.partial` file under `<base_path>/nar-partial/<peer_id>/<hash>` so an
/// interrupted upload can resume from a byte offset (issue #225) and a large
/// NAR no longer pins RAM. A per-session byte budget plus a poison set preserve
/// the #109 protection against a rogue worker opening many un-finalized streams
/// (the budget now bounds staged **disk**, not RAM). Keying by `peer_id` lets a
/// reconnecting worker resume its own partial without colliding with another
/// worker pushing the same content-addressed path.
pub(super) struct NarReceiveStore {
    store: gradient_storage::PartialStore,
    peer_id: String,
    max_bytes: u64,
    active: HashMap<String, PathState>,
    poisoned: BTreeSet<String>,
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

    fn key(&self, hash: &str) -> String {
        format!("{}/{}", self.peer_id, hash)
    }

    /// Record the push stream's token and return how many bytes are already
    /// staged for it (0 on token mismatch / nothing on disk). Clears any stale
    /// poison so a fresh attempt can proceed.
    pub(super) async fn note_header(&mut self, store_path: &str, token: &str) -> u64 {
        self.poisoned.remove(store_path);
        let received = match store_hash(store_path) {
            Some(h) => {
                let (store, key, token) = (self.store.clone(), self.key(h), token.to_owned());
                tokio::task::spawn_blocking(move || store.received_len(&key, &token).unwrap_or(0))
                    .await
                    .unwrap_or(0)
            }
            None => 0,
        };
        self.active.insert(
            store_path.to_owned(),
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
        store_path: &str,
        offset: u64,
        data: &[u8],
    ) -> AppendOutcome {
        if self.poisoned.contains(store_path) {
            return AppendOutcome::Poisoned;
        }

        let Some(hash) = store_hash(store_path) else {
            return AppendOutcome::Poisoned;
        };

        self.active.entry(store_path.to_owned()).or_default();
        let total: u64 = self.active.values().map(|s| s.staged).sum();
        if total.saturating_add(data.len() as u64) > self.max_bytes {
            self.poison(store_path, hash).await;
            return AppendOutcome::Overflow;
        }

        let token = self.active[store_path].token.clone();
        let (store, key, data) = (self.store.clone(), self.key(hash), data.to_vec());
        let len = data.len() as u64;
        match tokio::task::spawn_blocking(move || store.append(&key, &token, offset, &data)).await {
            Ok(Ok(())) => {
                if let Some(s) = self.active.get_mut(store_path) {
                    s.staged += len;
                }
                AppendOutcome::Ok
            }
            Ok(Err(e)) => {
                warn!(%store_path, error = %e, "partial append failed; poisoning path");
                self.poison(store_path, hash).await;
                AppendOutcome::Overflow
            }
            Err(e) => {
                warn!(%store_path, error = %e, "partial append task panicked; poisoning path");
                self.poison(store_path, hash).await;
                AppendOutcome::Overflow
            }
        }
    }

    async fn poison(&mut self, store_path: &str, hash: &str) {
        let (store, key) = (self.store.clone(), self.key(hash));
        let _ = tokio::task::spawn_blocking(move || store.discard(&key)).await;
        self.active.remove(store_path);
        self.poisoned.insert(store_path.to_owned());
    }

    /// A push stream is open for this path (direct mode). `false` means the
    /// worker uploaded via presigned S3 and there is nothing to commit.
    pub(super) fn is_active(&self, store_path: &str) -> bool {
        self.active.contains_key(store_path)
    }

    /// Actual on-disk length of the staged partial (validating the token).
    pub(super) async fn committed_len(&self, store_path: &str) -> u64 {
        let Some(hash) = store_hash(store_path) else {
            return 0;
        };
        let token = self
            .active
            .get(store_path)
            .map(|s| s.token.clone())
            .unwrap_or_default();
        let (store, key) = (self.store.clone(), self.key(hash));
        tokio::task::spawn_blocking(move || store.received_len(&key, &token).unwrap_or(0))
            .await
            .unwrap_or(0)
    }

    /// Read the staged bytes so the caller can commit them to `nar_storage`.
    pub(super) async fn read_staged(&self, store_path: &str) -> anyhow::Result<Vec<u8>> {
        let hash = store_hash(store_path)
            .ok_or_else(|| anyhow::anyhow!("malformed store path {store_path}"))?;
        let (store, key) = (self.store.clone(), self.key(hash));
        tokio::task::spawn_blocking(move || store.read_all(&key))
            .await
            .map_err(|e| anyhow::anyhow!("read staged NAR task panicked: {e}"))?
    }

    /// Drop the staged partial and per-path state after a successful commit.
    pub(super) async fn finish(&mut self, store_path: &str) {
        self.active.remove(store_path);
        if let Some(hash) = store_hash(store_path) {
            let (store, key) = (self.store.clone(), self.key(hash));
            let _ = tokio::task::spawn_blocking(move || store.discard(&key)).await;
        }
    }

    /// Has this path been poisoned by a prior overflow on the same session?
    pub(super) fn is_poisoned(&self, store_path: &str) -> bool {
        self.poisoned.contains(store_path)
    }

    /// Forget the poison flag and discard any partial for `store_path` so a
    /// later, well-formed retry of the same path can proceed.
    pub(super) async fn clear_poison(&mut self, store_path: &str) {
        self.poisoned.remove(store_path);
        self.finish(store_path).await;
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
        let received = nar.note_header(&store_path, &stream_token).await;
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
        match nar.append(&store_path, offset, &data).await {
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
        if nar.is_poisoned(&store_path) {
            nar.clear_poison(&store_path).await;
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

        let committed = if nar.is_active(&store_path) {
            self.commit_relayed(&job_id, &store_path, hash, &file_hash, file_size, nar)
                .await
        } else {
            self.commit_presigned(&job_id, &store_path, hash, file_size)
                .await
        };
        if !committed {
            return;
        }

        let file_size_i64 = file_size as i64;
        let nar_record = NarUploadRecord {
            file_hash: &file_hash,
            file_size: file_size_i64,
            nar_size: nar_size as i64,
            nar_hash: &nar_hash,
            references: &references,
            deriver: deriver.as_deref(),
        };
        if let Err(e) = mark_nar_stored(
            self.state,
            self.scheduler,
            &job_id,
            &store_path,
            &nar_record,
        )
        .await
        {
            warn!(%store_path, error = %e, "failed to mark NAR as stored");
        }
        if let Err(e) =
            record_nar_push_metric(self.state, self.scheduler, &job_id, file_size_i64).await
        {
            debug!(error = %e, "failed to record cache metric for NarUploaded");
        }
    }

    /// Commit a direct-mode (relayed) push: validate the staged partial's size
    /// against the reported `file_size`, write it to `nar_storage`, then drop
    /// the partial. Returns `false` (after failing the build transiently) if
    /// any step fails, in which case `on_nar_uploaded` must return early.
    async fn commit_relayed(
        &mut self,
        job_id: &str,
        store_path: &str,
        hash: &str,
        file_hash: &str,
        file_size: u64,
        nar: &mut NarReceiveStore,
    ) -> bool {
        let staged = nar.committed_len(store_path).await;
        if staged != file_size {
            let reason =
                format!("staged NAR size {staged} does not match reported file_size {file_size}");
            error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "NAR upload integrity check failed");
            self.fail_build_transient(job_id, reason).await;
            return false;
        }
        let buf = match nar.read_staged(store_path).await {
            Ok(b) => b,
            Err(e) => {
                let reason = format!("failed to read staged NAR: {e}");
                error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "read staged NAR failed");
                self.fail_build_transient(job_id, reason).await;
                return false;
            }
        };
        if let Err(e) = crate::ingest::put_nar_idempotent(
            &self.state.worker_db,
            &self.state.nar_storage,
            hash,
            file_hash,
            buf,
        )
        .await
        {
            let reason = format!("failed to write NAR to storage: {e}");
            error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "nar_storage.put failed");
            self.fail_build_transient(job_id, reason).await;
            return false;
        }
        nar.finish(store_path).await;
        debug!(peer_id = %self.peer_id, %job_id, %store_path, file_size, "NAR stored");
        true
    }

    /// Commit a presigned (S3) upload: the worker already PUT the bytes
    /// directly, so HEAD the object and compare its size against the reported
    /// `file_size`. Returns `false` (after failing the build transiently) if
    /// the object is missing or its size mismatches.
    async fn commit_presigned(
        &mut self,
        job_id: &str,
        store_path: &str,
        hash: &str,
        file_size: u64,
    ) -> bool {
        match self.state.nar_storage.head_size(hash).await {
            Ok(Some(size)) if size == file_size => true,
            Ok(Some(size)) => {
                let reason = format!(
                    "presigned NAR object size {size} does not match reported file_size {file_size}"
                );
                error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "presigned NAR upload integrity check failed");
                self.fail_build_transient(job_id, reason).await;
                false
            }
            Ok(None) => {
                let reason =
                    format!("presigned NAR object for {store_path} is missing from storage");
                error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "presigned NAR upload integrity check failed");
                self.fail_build_transient(job_id, reason).await;
                false
            }
            Err(e) => {
                let reason = format!("failed to head presigned NAR object: {e}");
                error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "presigned NAR HEAD failed");
                self.fail_build_transient(job_id, reason).await;
                false
            }
        }
    }

    /// Send `AbortJob` to the worker. Used when a NAR upload cannot be
    /// committed safely - the worker stops the job and replies with
    /// `JobFailed`, which the scheduler turns into a failed build.
    async fn abort_job(&mut self, job_id: &str, reason: String) {
        let _ = send_server_msg(
            self.writer,
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
    /// `abort_job` would be reported by the worker as a permanent failure and
    /// never retry. The connection is untouched; only this build fails.
    async fn fail_build_transient(&mut self, job_id: &str, reason: String) {
        self.abort_job(job_id, reason.clone()).await;
        if let Err(e) = self
            .scheduler
            .handle_job_failed(
                self.peer_id,
                job_id,
                &reason,
                gradient_types::proto::BuildFailureKind::Transient,
                &[],
            )
            .await
        {
            error!(peer_id = %self.peer_id, %job_id, error = %e, "fail_build_transient: handle_job_failed failed");
        }
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

    #[tokio::test]
    async fn append_below_budget_stages_and_reads_back() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        assert_ok(s.append(&a, 0, &[0u8; 256]).await);
        assert_ok(s.append(&a, 256, &[1u8; 256]).await);
        assert_eq!(s.committed_len(&a).await, 512);
        assert_eq!(s.read_staged(&a).await.unwrap().len(), 512);
    }

    #[tokio::test]
    async fn non_contiguous_offset_poisons_path() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        assert_ok(s.append(&a, 0, &[0u8; 100]).await);
        assert!(matches!(
            s.append(&a, 999, &[0u8; 10]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(&a));
        assert!(matches!(
            s.append(&a, 0, &[0u8; 10]).await,
            AppendOutcome::Poisoned
        ));
    }

    #[tokio::test]
    async fn append_overflow_poisons_path() {
        let (_d, mut s) = store(1024);
        let a = path('a');
        assert_ok(s.append(&a, 0, &[0u8; 1000]).await);
        assert!(matches!(
            s.append(&a, 1000, &[0u8; 100]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(&a));
        assert!(matches!(
            s.append(&a, 0, &[0u8; 50]).await,
            AppendOutcome::Poisoned
        ));
    }

    #[tokio::test]
    async fn overflow_across_keys_is_caught() {
        let (_d, mut s) = store(800);
        assert_ok(s.append(&path('a'), 0, &[0u8; 400]).await);
        assert_ok(s.append(&path('b'), 0, &[0u8; 400]).await);
        assert!(matches!(
            s.append(&path('c'), 0, &[42u8]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(&path('c')));
    }

    #[tokio::test]
    async fn note_header_reports_resumable_prefix() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        s.note_header(&a, "tok-v1").await;
        assert_ok(s.append(&a, 0, b"hello").await);
        // Simulated reconnect: same token resumes; a different token restarts.
        assert_eq!(s.note_header(&a, "tok-v1").await, 5);
        assert_eq!(s.note_header(&a, "tok-v2").await, 0);
    }

    #[test]
    fn presigned_mode_has_no_active_stream() {
        let (_d, s) = store(1024);
        assert!(
            !s.is_active(&path('a')),
            "a path with no header/push must not be treated as direct mode"
        );
    }

    #[tokio::test]
    async fn clear_poison_allows_retry() {
        let (_d, mut s) = store(100);
        let a = path('a');
        assert!(matches!(
            s.append(&a, 0, &[0u8; 200]).await,
            AppendOutcome::Overflow
        ));
        assert!(s.is_poisoned(&a));
        s.clear_poison(&a).await;
        assert!(!s.is_poisoned(&a));
        assert_ok(s.append(&a, 0, &[0u8; 50]).await);
    }

    #[tokio::test]
    async fn finish_discards_staged_partial() {
        let (_d, mut s) = store(10_000);
        let a = path('a');
        assert_ok(s.append(&a, 0, b"hello").await);
        s.finish(&a).await;
        assert!(!s.is_active(&a));
        assert_eq!(s.committed_len(&a).await, 0);
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
