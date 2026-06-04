/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-connection message dispatch context and all `ClientMessage` handlers.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gradient_core::executer::strip_nix_store_prefix;
use gradient_core::types::ids::{DerivationId, OrganizationId};
use gradient_core::types::*;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::messages::{CandidateScore, ClientMessage, JobKind, JobUpdateKind, ServerMessage};
use scheduler::Scheduler;

use super::auth::{lookup_registered_peers, validate_tokens};
use super::cache::handle_cache_query;
use super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoWriter, push_pending_candidates, send_credentials_for_job,
    send_error, send_server_msg, serve_nar_request,
};

// ── Per-session inbound NAR buffer (issue #109) ──────────────────────────────

/// Outcome of [`NarBuffers::append`].
pub(super) enum AppendOutcome {
    /// Chunk was appended.
    Ok,
    /// Chunk would have exceeded the session budget. The path is now poisoned
    /// and any partial buffer for it has been dropped - the caller must abort
    /// the job and refuse the eventual `NarUploaded` for the same path.
    Overflow,
    /// Chunk arrived for a path the session has already poisoned. Drop it.
    Poisoned,
}

/// Bounded buffer pool for inbound `NarPush` chunks. Tracks total queued bytes
/// and rejects pushes that would exceed `max_bytes` so a rogue worker cannot
/// pin unbounded RAM by opening many concurrent uploads with no `is_final`.
///
/// Once a path overflows the budget it is added to a per-session **poison
/// set**. Any further chunks for that path are dropped, and the eventual
/// `NarUploaded` for it must be rejected - otherwise the server would write a
/// `cached_path` row with metadata for bytes that were never persisted to
/// `nar_storage`, and downstream builds would later fail with
/// "NAR not found in cache" for a path the DB swears is cached.
pub(super) struct NarBuffers {
    inner: HashMap<String, Vec<u8>>,
    /// Paths whose upload was rejected and must not be committed.
    poisoned: BTreeSet<String>,
    total_bytes: usize,
    max_bytes: usize,
}

impl NarBuffers {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            inner: HashMap::new(),
            poisoned: BTreeSet::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    /// Append `data` to the buffer for `store_path`.
    ///
    /// Returns:
    /// - [`AppendOutcome::Ok`] if the chunk was appended.
    /// - [`AppendOutcome::Overflow`] if the chunk would have exceeded the
    ///   session budget. The path is now poisoned and any partial buffer for
    ///   it has been dropped.
    /// - [`AppendOutcome::Poisoned`] if the path has already been poisoned by
    ///   a prior overflow.
    pub(super) fn append(&mut self, store_path: &str, data: &[u8]) -> AppendOutcome {
        if self.poisoned.contains(store_path) {
            return AppendOutcome::Poisoned;
        }
        if self.total_bytes.saturating_add(data.len()) > self.max_bytes {
            self.discard(store_path);
            self.poisoned.insert(store_path.to_owned());
            return AppendOutcome::Overflow;
        }
        self.inner
            .entry(store_path.to_owned())
            .or_default()
            .extend_from_slice(data);
        self.total_bytes += data.len();
        AppendOutcome::Ok
    }

    /// Pop the assembled buffer for `store_path`. Returns `None` if no chunks
    /// were ever buffered (e.g. presigned-S3 path that bypassed `NarPush`).
    /// Poisoned paths return `None` here too - callers must check
    /// [`Self::is_poisoned`] before treating `None` as "S3 mode, accept the
    /// metadata".
    pub(super) fn take(&mut self, store_path: &str) -> Option<Vec<u8>> {
        let buf = self.inner.remove(store_path)?;
        self.total_bytes = self.total_bytes.saturating_sub(buf.len());
        Some(buf)
    }

    /// Drop the partial buffer for `store_path` without consuming it.
    fn discard(&mut self, store_path: &str) {
        if let Some(buf) = self.inner.remove(store_path) {
            self.total_bytes = self.total_bytes.saturating_sub(buf.len());
        }
    }

    /// Has this path been poisoned by a prior overflow on the same session?
    pub(super) fn is_poisoned(&self, store_path: &str) -> bool {
        self.poisoned.contains(store_path)
    }

    /// Forget the poison flag for `store_path`. Called after the server has
    /// rejected the upload so a later, well-formed retry of the same path
    /// (e.g. on a fresh job) can proceed.
    pub(super) fn clear_poison(&mut self, store_path: &str) {
        self.poisoned.remove(store_path);
    }

    pub(super) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub(super) fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

// ── Dispatch context ──────────────────────────────────────────────────────────

/// Holds the per-connection references needed to handle a single client message.
pub(super) struct DispatchContext<'a> {
    pub writer: &'a ProtoWriter,
    pub state: &'a Arc<ServerState>,
    pub scheduler: &'a Arc<Scheduler>,
    pub peer_id: &'a str,
    /// Bounds the number of NAR-serving tasks running concurrently per
    /// connection. Cloned into each spawned `serve_nar_request` task.
    pub nar_serve_semaphore: &'a Arc<Semaphore>,
}

impl<'a> DispatchContext<'a> {
    /// Route a single `ClientMessage` to the appropriate handler.
    ///
    /// Returns `true` to continue the loop, `false` to break.
    pub async fn dispatch(&mut self, msg: ClientMessage, nar_buffers: &mut NarBuffers) -> bool {
        // Avoid Debug-printing the entire `msg` here: variants like `NarPush`
        // carry up to 64 KiB of binary chunk data which would flood the log
        // (and the test VM's serial console). Each match arm logs the
        // semantically interesting fields itself.
        debug!(variant = msg.variant_name(), "received client message");
        match msg {
            ClientMessage::InitConnection { .. } => {
                send_error(self.writer, 400, "unexpected InitConnection".into()).await;
                false
            }
            ClientMessage::Reject { code, reason } => {
                info!(peer_id = %self.peer_id, code, %reason, "peer rejected connection");
                false
            }
            ClientMessage::ReauthRequest => self.on_reauth_request().await,
            ClientMessage::AuthResponse { tokens } => self.on_auth_response(tokens).await,
            ClientMessage::WorkerCapabilities {
                architectures,
                system_features,
                max_concurrent_builds,
                cpu_count,
                ram_total_mb,
                cpu_core_score,
            } => {
                self.on_worker_capabilities(
                    architectures,
                    system_features,
                    max_concurrent_builds,
                    cpu_count,
                    ram_total_mb,
                    cpu_core_score,
                )
                .await;
                true
            }
            ClientMessage::WorkerMetrics {
                cpu_usage_pct,
                ram_free_mb,
                disk_speed_mbps,
            } => {
                self.on_worker_metrics(cpu_usage_pct, ram_free_mb, disk_speed_mbps)
                    .await;
                true
            }
            ClientMessage::RequestJobList => self.on_request_job_list().await,
            ClientMessage::RequestJob { kind } => self.on_request_job(kind).await,
            ClientMessage::RequestAllCandidates => self.on_request_all_candidates().await,
            ClientMessage::RequestJobChunk { scores, is_final } => {
                self.on_request_job_chunk(scores, is_final).await;
                true
            }
            ClientMessage::AssignJobResponse {
                job_id,
                accepted,
                reason,
            } => {
                self.on_assign_job_response(job_id, accepted, reason).await;
                true
            }
            ClientMessage::JobUpdate { job_id, update } => {
                self.on_job_update(job_id, update).await;
                true
            }
            ClientMessage::JobCompleted { job_id, metrics } => {
                self.on_job_completed(job_id, metrics).await;
                true
            }
            ClientMessage::JobFailed {
                job_id,
                error,
                kind,
            } => {
                self.on_job_failed(job_id, error, kind).await;
                true
            }
            ClientMessage::Draining => {
                self.on_draining().await;
                true
            }
            ClientMessage::LogChunk {
                job_id,
                task_index,
                data,
            } => {
                self.on_log_chunk(job_id, task_index, data).await;
                true
            }
            ClientMessage::NarRequest { job_id, paths } => {
                self.on_nar_request(job_id, paths).await;
                true
            }
            ClientMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                self.on_nar_push(job_id, store_path, data, offset, is_final, nar_buffers)
                    .await;
                true
            }
            ClientMessage::NarUploaded {
                job_id,
                store_path,
                file_hash,
                file_size,
                nar_size,
                nar_hash,
                references,
                deriver,
            } => {
                self.on_nar_uploaded(
                    job_id,
                    store_path,
                    file_hash,
                    file_size,
                    nar_size,
                    nar_hash,
                    references,
                    deriver,
                    nar_buffers,
                )
                .await;
                true
            }
            ClientMessage::CacheQuery {
                job_id,
                paths,
                mode,
            } => self.on_cache_query(job_id, paths, mode).await,
            ClientMessage::QueryKnownDerivations { job_id, drv_paths } => {
                self.on_query_known_derivations(job_id, drv_paths).await
            }
            ClientMessage::EvalMessage {
                job_id,
                level,
                source,
                message,
            } => {
                self.on_eval_message(job_id, level, source, message).await;
                true
            }
        }
    }

    async fn on_eval_message(
        &mut self,
        job_id: String,
        level: gradient_core::types::proto::EvalMessageLevel,
        source: String,
        message: String,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, ?level, %source, "EvalMessage");
        if let Err(e) = self
            .scheduler
            .record_eval_message(&job_id, level, source, message)
            .await
        {
            warn!(peer_id = %self.peer_id, %job_id, error = %e, "record_eval_message failed");
        }
    }

    // ── Reauth ────────────────────────────────────────────────────────────────

    async fn on_reauth_request(&mut self) -> bool {
        debug!(peer_id = %self.peer_id, "ReauthRequest");
        let registered_peers = lookup_registered_peers(self.state, self.peer_id).await;
        send_server_msg(
            self.writer,
            &ServerMessage::AuthChallenge {
                peers: registered_peers.iter().map(|(id, _)| id.clone()).collect(),
            },
        )
        .await
        .is_ok()
    }

    async fn on_auth_response(&mut self, tokens: Vec<(String, String)>) -> bool {
        let registered_peers = lookup_registered_peers(self.state, self.peer_id).await;
        let (authorized_peers, failed_peers) = validate_tokens(&registered_peers, &tokens);
        let updated_uuids: HashSet<OrganizationId> = authorized_peers
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        self.scheduler
            .update_authorized_peers(self.peer_id, updated_uuids)
            .await;
        send_server_msg(
            self.writer,
            &ServerMessage::AuthUpdate {
                authorized_peers,
                failed_peers,
            },
        )
        .await
        .is_ok()
    }

    // ── Capability advertisement ──────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)] // mirrors the WorkerCapabilities wire fields
    async fn on_worker_capabilities(
        &mut self,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
        cpu_count: u32,
        ram_total_mb: u64,
        cpu_core_score: u32,
    ) {
        debug!(peer_id = %self.peer_id, ?architectures, ?system_features, max_concurrent_builds, cpu_count, ram_total_mb, cpu_core_score, "WorkerCapabilities");
        self.scheduler
            .update_worker_capabilities(
                self.peer_id,
                architectures,
                system_features,
                max_concurrent_builds,
                cpu_count,
                ram_total_mb,
                cpu_core_score,
            )
            .await;
    }

    async fn on_worker_metrics(
        &mut self,
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
    ) {
        debug!(peer_id = %self.peer_id, cpu_usage_pct, ram_free_mb, ?disk_speed_mbps, "WorkerMetrics");
        self.scheduler
            .update_worker_metrics(self.peer_id, cpu_usage_pct, ram_free_mb, disk_speed_mbps)
            .await;
    }

    // ── Job list / scoring ────────────────────────────────────────────────────

    async fn on_request_job_list(&mut self) -> bool {
        debug!(peer_id = %self.peer_id, "RequestJobList");
        let candidates = self.scheduler.get_job_candidates(self.peer_id).await;
        self.send_job_list_chunks(candidates).await
    }

    async fn on_request_all_candidates(&mut self) -> bool {
        debug!(peer_id = %self.peer_id, "RequestAllCandidates");
        let candidates = self.scheduler.get_job_candidates(self.peer_id).await;
        self.send_job_list_chunks(candidates).await
    }

    async fn send_job_list_chunks(
        &mut self,
        candidates: Vec<crate::messages::JobCandidate>,
    ) -> bool {
        use crate::messages::ServerMessage;
        let chunks: Vec<_> = candidates.chunks(JOB_OFFER_CHUNK_SIZE).collect();
        let total = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            if send_server_msg(
                self.writer,
                &ServerMessage::JobListChunk {
                    candidates: chunk.to_vec(),
                    is_final: i + 1 == total,
                },
            )
            .await
            .is_err()
            {
                return false;
            }
        }
        if total == 0 {
            return send_server_msg(
                self.writer,
                &ServerMessage::JobListChunk {
                    candidates: vec![],
                    is_final: true,
                },
            )
            .await
            .is_ok();
        }
        true
    }

    // ── Job request ───────────────────────────────────────────────────────────

    async fn on_request_job(&mut self, kind: JobKind) -> bool {
        debug!(peer_id = %self.peer_id, ?kind, "RequestJob");
        if let Some(assignment) = self.scheduler.request_job(self.peer_id, kind).await {
            send_credentials_for_job(
                self.writer,
                self.state,
                self.scheduler,
                self.peer_id,
                &assignment.job,
                assignment.peer_id,
            )
            .await;
            if send_server_msg(
                self.writer,
                &ServerMessage::AssignJob {
                    job_id: assignment.job_id,
                    job: assignment.job,
                    timeout_secs: Some(3600),
                },
            )
            .await
            .is_err()
            {
                return false;
            }
        }
        true
    }

    // ── Scoring ───────────────────────────────────────────────────────────────

    async fn on_request_job_chunk(&mut self, scores: Vec<CandidateScore>, is_final: bool) {
        debug!(peer_id = %self.peer_id, count = scores.len(), is_final, "RequestJobChunk");
        self.scheduler.record_scores(self.peer_id, scores).await;
    }

    // ── Job accept / reject ───────────────────────────────────────────────────

    async fn on_assign_job_response(
        &mut self,
        job_id: String,
        accepted: bool,
        reason: Option<String>,
    ) {
        if accepted {
            info!(peer_id = %self.peer_id, %job_id, "job accepted");
        } else {
            info!(peer_id = %self.peer_id, %job_id, ?reason, "job rejected by worker");
            self.scheduler.job_rejected(self.peer_id, &job_id).await;
        }
    }

    // ── Progress updates ──────────────────────────────────────────────────────

    async fn on_job_update(&mut self, job_id: String, update: JobUpdateKind) {
        debug!(peer_id = %self.peer_id, %job_id, ?update, "JobUpdate");
        match update {
            JobUpdateKind::Fetching => {
                self.scheduler
                    .handle_eval_status_update(
                        &job_id,
                        entity::evaluation::EvaluationStatus::Fetching,
                    )
                    .await;
            }
            JobUpdateKind::FetchResult { flake_source } => {
                debug!(peer_id = %self.peer_id, %job_id, ?flake_source, "FetchResult");
                self.scheduler
                    .persist_flake_source(&job_id, flake_source)
                    .await;
            }
            JobUpdateKind::EvaluatingFlake => {
                self.scheduler
                    .handle_eval_status_update(
                        &job_id,
                        entity::evaluation::EvaluationStatus::EvaluatingFlake,
                    )
                    .await;
            }
            JobUpdateKind::EvaluatingDerivations => {
                self.scheduler
                    .handle_eval_status_update(
                        &job_id,
                        entity::evaluation::EvaluationStatus::EvaluatingDerivation,
                    )
                    .await;
            }
            JobUpdateKind::EvalResult {
                derivations,
                warnings,
                errors,
            } => {
                if let Err(e) = self
                    .scheduler
                    .handle_eval_result(&job_id, derivations, warnings, errors)
                    .await
                {
                    error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_eval_result failed");
                }
                push_pending_candidates(self.writer, self.scheduler, self.peer_id).await;
            }
            JobUpdateKind::Building { build_id } => {
                self.scheduler
                    .handle_build_status_update(&build_id, self.peer_id)
                    .await;
            }
            JobUpdateKind::BuildOutput { build_id, outputs } => {
                if let Err(e) = self
                    .scheduler
                    .handle_build_output(&job_id, &build_id, outputs)
                    .await
                {
                    error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_build_output failed");
                }
            }
            JobUpdateKind::Compressing => {}
        }
    }

    // ── Job terminal states ───────────────────────────────────────────────────

    async fn on_job_completed(
        &mut self,
        job_id: String,
        metrics: Option<gradient_core::types::proto::BuildMetrics>,
    ) {
        info!(peer_id = %self.peer_id, %job_id, "job completed");
        if let Some(m) = &metrics {
            debug!(peer_id = %self.peer_id, %job_id, ?m, "received build metrics");
        }
        if let Err(e) = self
            .scheduler
            .handle_job_completed(self.peer_id, &job_id, metrics)
            .await
        {
            error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_job_completed failed");
        }
        push_pending_candidates(self.writer, self.scheduler, self.peer_id).await;
    }

    async fn on_job_failed(
        &mut self,
        job_id: String,
        error: String,
        kind: gradient_core::types::proto::BuildFailureKind,
    ) {
        warn!(peer_id = %self.peer_id, %job_id, %error, ?kind, "job failed");
        if let Err(e) = self
            .scheduler
            .handle_job_failed(self.peer_id, &job_id, &error, kind)
            .await
        {
            error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_job_failed failed");
        }
        push_pending_candidates(self.writer, self.scheduler, self.peer_id).await;
    }

    // ── Worker draining ───────────────────────────────────────────────────────

    async fn on_draining(&mut self) {
        info!(peer_id = %self.peer_id, "worker draining");
        self.scheduler.mark_worker_draining(self.peer_id).await;
    }

    // ── Log streaming ─────────────────────────────────────────────────────────

    async fn on_log_chunk(&mut self, job_id: String, task_index: u32, data: Vec<u8>) {
        debug!(peer_id = %self.peer_id, %job_id, task_index, bytes = data.len(), "LogChunk");
        if let Err(e) = self.scheduler.append_log(&job_id, task_index, data).await {
            debug!(peer_id = %self.peer_id, %job_id, error = %e, "log append failed");
        }
    }

    // ── NAR transfer ──────────────────────────────────────────────────────────

    async fn on_nar_request(&mut self, job_id: String, paths: Vec<String>) {
        debug!(peer_id = %self.peer_id, %job_id, count = paths.len(), "NarRequest");
        // Spawn one task per path so a slow storage read for path[0] does not
        // serialise paths[1..]. The shared `nar_serve_semaphore` caps fan-out
        // per connection, and the cloneable `ProtoWriter` interleaves chunks
        // safely on the wire (the worker keys NarPush by store_path).
        for store_path in paths {
            let state = Arc::clone(self.state);
            let writer = self.writer.clone();
            let permit = Arc::clone(self.nar_serve_semaphore);
            let peer_id = self.peer_id.to_owned();
            let job_id = job_id.clone();
            tokio::spawn(async move {
                let _guard = match permit.acquire_owned().await {
                    Ok(g) => g,
                    Err(_) => return, // semaphore closed (shutdown)
                };
                if let Err(e) = serve_nar_request(&state, &writer, &job_id, &store_path).await {
                    warn!(%peer_id, %job_id, %store_path, error = %e, "NarRequest serve failed");
                }
            });
        }
    }

    async fn on_nar_push(
        &mut self,
        job_id: String,
        store_path: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
        nar_buffers: &mut NarBuffers,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");
        if data.is_empty() {
            return;
        }
        match nar_buffers.append(&store_path, &data) {
            AppendOutcome::Ok => {}
            AppendOutcome::Overflow => {
                let reason = format!(
                    "session NAR upload buffer would exceed {} bytes (current {} + {} = {})",
                    nar_buffers.max_bytes(),
                    nar_buffers.total_bytes(),
                    data.len(),
                    nar_buffers.total_bytes().saturating_add(data.len()),
                );
                warn!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "session NAR upload buffer would exceed limit; poisoning path");
                self.abort_job(&job_id, reason).await;
            }
            AppendOutcome::Poisoned => {
                debug!(peer_id = %self.peer_id, %job_id, %store_path, "discarding NarPush chunk for poisoned path");
            }
        }
        // The buffer is held until `on_nar_uploaded` arrives; that handler
        // commits it to `nar_storage` and records the metadata atomically so
        // we never end up with a `cached_path` row claiming bytes that
        // aren't actually stored.
    }

    /// Apply the worker's NAR upload metadata.
    ///
    /// For local-mode pushes (preceded by `NarPush` chunks), the buffered
    /// bytes are popped from `nar_buffers`, validated against the reported
    /// `file_size`, written to `nar_storage`, and only then is
    /// `mark_nar_stored` invoked. Any failure aborts the job with
    /// [`ServerMessage::AbortJob`] so the build is marked failed and the
    /// scheduler does not advertise the path as cached.
    ///
    /// For S3 / presigned uploads (no preceding `NarPush`), there is no
    /// buffer to commit - the worker has already PUT the bytes directly to
    /// object storage and we just record the metadata.
    #[allow(clippy::too_many_arguments)] // mirrors the wire-protocol message fields
    async fn on_nar_uploaded(
        &mut self,
        job_id: String,
        store_path: String,
        file_hash: String,
        file_size: u64,
        nar_size: u64,
        nar_hash: String,
        references: Vec<String>,
        deriver: Option<String>,
        nar_buffers: &mut NarBuffers,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, %file_hash, file_size, nar_size, %nar_hash, ?deriver, "NarUploaded");

        // Reject any NarUploaded for a path whose chunked transfer was
        // rejected mid-stream. Without this guard `nar_buffers.take()` would
        // return `None` (the buffer was discarded on overflow), the local-
        // mode commit block would be skipped, and `mark_nar_stored` would
        // record a `cached_path` row whose bytes never reached `nar_storage`
        // - leaving the path "cached" in the DB and undeliverable on the
        // next download.
        if nar_buffers.is_poisoned(&store_path) {
            nar_buffers.clear_poison(&store_path);
            let reason = format!(
                "NarUploaded for {store_path} rejected: prior NarPush chunk \
                 exceeded the session buffer budget"
            );
            warn!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "rejecting NarUploaded for poisoned path");
            self.abort_job(&job_id, reason).await;
            return;
        }

        if let Some(buf) = nar_buffers.take(&store_path) {
            if buf.len() as u64 != file_size {
                let reason = format!(
                    "NarPush buffer size {} does not match reported file_size {}",
                    buf.len(),
                    file_size
                );
                error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "NAR upload integrity check failed");
                self.abort_job(&job_id, reason).await;
                return;
            }
            let hash = store_path
                .strip_prefix("/nix/store/")
                .unwrap_or(&store_path)
                .split('-')
                .next()
                .unwrap_or("");
            let valid = hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric());
            if !valid {
                let reason = format!("NarUploaded for malformed store path {store_path}");
                error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "NarUploaded for malformed store path");
                self.abort_job(&job_id, reason).await;
                return;
            }
            if let Err(e) = self.state.nar_storage.put(hash, buf).await {
                let reason = format!("failed to write NAR to storage: {e}");
                error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "nar_storage.put failed");
                self.abort_job(&job_id, reason).await;
                return;
            }
            info!(peer_id = %self.peer_id, %job_id, %store_path, file_size, "NAR stored");
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
        if let Err(e) =
            mark_nar_stored(self.state, self.scheduler, &job_id, &store_path, &nar_record).await
        {
            warn!(%store_path, error = %e, "failed to mark NAR as stored");
        }
        if let Err(e) =
            record_nar_push_metric(self.state, self.scheduler, &job_id, file_size_i64).await
        {
            debug!(error = %e, "failed to record cache metric for NarUploaded");
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

    // ── Cache queries ─────────────────────────────────────────────────────────

    async fn on_cache_query(
        &mut self,
        job_id: String,
        paths: Vec<String>,
        mode: gradient_core::types::proto::QueryMode,
    ) -> bool {
        debug!(peer_id = %self.peer_id, %job_id, count = paths.len(), ?mode, "CacheQuery");
        let org_id = self.scheduler.peer_id_for_job(&job_id).await;
        let cached = handle_cache_query(self.state, org_id, &paths, mode).await;
        debug!(peer_id = %self.peer_id, %job_id, entries = cached.len(), "CacheStatus");
        send_server_msg(self.writer, &ServerMessage::CacheStatus { job_id, cached })
            .await
            .is_ok()
    }

    async fn on_query_known_derivations(&mut self, job_id: String, drv_paths: Vec<String>) -> bool {
        debug!(peer_id = %self.peer_id, %job_id, count = drv_paths.len(), "QueryKnownDerivations");
        let stripped: Vec<String> = drv_paths
            .iter()
            .map(|p| strip_nix_store_prefix(p))
            .collect();
        let hashes: Vec<String> = stripped
            .iter()
            .filter_map(|p| {
                gradient_core::sources::parse_drv_hash_name(p)
                    .ok()
                    .map(|(h, _)| h)
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let known = match self.scheduler.peer_id_for_job(&job_id).await {
            Some(org_id) => {
                use entity::build::BuildStatus;
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

                // First: find derivations that exist for this org.
                let candidates = EDerivation::find()
                    .filter(CDerivation::Organization.eq(org_id))
                    .filter(CDerivation::Hash.is_in(hashes))
                    .all(&self.state.worker_db)
                    .await
                    .unwrap_or_default();

                if candidates.is_empty() {
                    vec![]
                } else {
                    // Second: keep only those that have a Completed or Substituted build.
                    // A derivation exists in the DB but has only Failed builds should
                    // NOT be pruned - the worker must retry it.
                    let drv_ids: Vec<DerivationId> = candidates.iter().map(|d| d.id).collect();
                    let built: std::collections::HashSet<DerivationId> = EBuild::find()
                        .filter(CBuild::Derivation.is_in(drv_ids))
                        .filter(
                            CBuild::Status
                                .is_in(vec![BuildStatus::Completed, BuildStatus::Substituted]),
                        )
                        .all(&self.state.worker_db)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|b| b.derivation)
                        .collect();

                    candidates
                        .into_iter()
                        .filter(|d| built.contains(&d.id))
                        .map(|d| d.store_path())
                        .collect()
                }
            }
            None => {
                warn!(peer_id = %self.peer_id, %job_id, "QueryKnownDerivations: no org for job");
                vec![]
            }
        };
        debug!(peer_id = %self.peer_id, %job_id, known = known.len(), "KnownDerivations");
        send_server_msg(
            self.writer,
            &ServerMessage::KnownDerivations { job_id, known },
        )
        .await
        .is_ok()
    }
}

#[cfg(test)]
mod nar_buffers_tests {
    use super::{AppendOutcome, NarBuffers};

    fn assert_ok(o: AppendOutcome) {
        assert!(matches!(o, AppendOutcome::Ok), "expected Ok");
    }

    #[test]
    fn append_below_budget_succeeds_and_tracks_total() {
        let mut nb = NarBuffers::new(1024);
        assert_ok(nb.append("/nix/store/a", &vec![0u8; 256]));
        assert_ok(nb.append("/nix/store/a", &vec![1u8; 256]));
        assert_ok(nb.append("/nix/store/b", &vec![2u8; 256]));
        assert_eq!(nb.total_bytes(), 768);
    }

    #[test]
    fn append_overflow_drops_partial_buffer_and_poisons_path() {
        let mut nb = NarBuffers::new(1024);
        assert_ok(nb.append("/nix/store/a", &vec![0u8; 1000]));
        // Second chunk would exceed budget - overflow drops the partial
        // buffer for /a and marks it poisoned so the eventual NarUploaded is
        // rejected.
        assert!(matches!(
            nb.append("/nix/store/a", &[0u8; 100]),
            AppendOutcome::Overflow
        ));
        assert!(nb.is_poisoned("/nix/store/a"));
        assert_eq!(
            nb.total_bytes(),
            0,
            "overflow must release the partial buffer's bytes back to the budget"
        );
        // Subsequent chunks for the same path are dropped silently.
        assert!(matches!(
            nb.append("/nix/store/a", &[0u8; 50]),
            AppendOutcome::Poisoned
        ));
        assert!(
            nb.take("/nix/store/a").is_none(),
            "take() must not return a buffer for a poisoned path"
        );
    }

    #[test]
    fn take_releases_budget() {
        let mut nb = NarBuffers::new(1024);
        assert_ok(nb.append("/nix/store/a", &vec![0u8; 500]));
        assert_ok(nb.append("/nix/store/b", &vec![1u8; 500]));
        let buf_a = nb.take("/nix/store/a").expect("a was buffered");
        assert_eq!(buf_a.len(), 500);
        assert_eq!(nb.total_bytes(), 500);
        assert_ok(nb.append("/nix/store/c", &vec![2u8; 400]));
    }

    #[test]
    fn take_missing_returns_none() {
        let mut nb = NarBuffers::new(1024);
        assert!(nb.take("/nix/store/missing").is_none());
    }

    #[test]
    fn append_overflow_across_keys_is_caught() {
        let mut nb = NarBuffers::new(800);
        assert_ok(nb.append("/nix/store/a", &vec![0u8; 400]));
        assert_ok(nb.append("/nix/store/b", &vec![0u8; 400]));
        assert!(matches!(
            nb.append("/nix/store/c", &[42u8]),
            AppendOutcome::Overflow
        ));
        assert!(nb.is_poisoned("/nix/store/c"));
    }

    #[test]
    fn clear_poison_allows_retry() {
        let mut nb = NarBuffers::new(100);
        assert!(matches!(
            nb.append("/nix/store/a", &[0u8; 200]),
            AppendOutcome::Overflow
        ));
        assert!(nb.is_poisoned("/nix/store/a"));
        nb.clear_poison("/nix/store/a");
        assert!(!nb.is_poisoned("/nix/store/a"));
        assert_ok(nb.append("/nix/store/a", &[0u8; 50]));
    }
}
