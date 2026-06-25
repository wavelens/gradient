/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-connection message dispatch context and all `ClientMessage` handlers.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gradient_exec::strip_nix_store_prefix;
use gradient_types::ids::{DerivationId, OrganizationId};
use gradient_types::*;
use gradient_core::ServerState;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::messages::{CandidateScore, ClientMessage, JobKind, JobUpdateKind, ServerMessage};
use gradient_scheduler::Scheduler;

use super::auth::{
    expand_base_authorized, lookup_base_worker_challenge, lookup_registered_peers, validate_tokens,
};
use super::cache::handle_cache_query;
use super::eval_cache::{
    EvalCacheReceiveStore, handle_eval_cache_chunk, handle_eval_cache_pull, handle_eval_cache_push,
    handle_eval_cache_push_done,
};
use super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoWriter, push_pending_candidates, send_credentials_for_job,
    send_error, send_server_msg, serve_nar_request,
};

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
    pub async fn dispatch(
        &mut self,
        msg: ClientMessage,
        nar: &mut NarReceiveStore,
        eval_cache: &mut EvalCacheReceiveStore,
    ) -> bool {
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
                network_speed_mbps,
            } => {
                self.on_worker_metrics(cpu_usage_pct, ram_free_mb, disk_speed_mbps, network_speed_mbps)
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
            ClientMessage::JobCompleted { job_id } => {
                self.on_job_completed(job_id).await;
                true
            }
            ClientMessage::JobFailed {
                job_id,
                error,
                kind,
                missing_paths,
            } => {
                self.on_job_failed(job_id, error, kind, missing_paths).await;
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
            ClientMessage::NarRequestResume {
                job_id,
                store_path,
                received_bytes,
                stream_token,
            } => {
                self.on_nar_request_resume(job_id, store_path, received_bytes, stream_token)
                    .await;
                true
            }
            ClientMessage::NarStreamHeader {
                job_id,
                store_path,
                total_bytes,
                stream_token,
            } => {
                self.on_push_stream_header(job_id, store_path, total_bytes, stream_token, nar)
                    .await;
                true
            }
            ClientMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                self.on_nar_push(job_id, store_path, data, offset, is_final, nar)
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
                    nar,
                )
                .await;
                true
            }
            ClientMessage::EvalCachePull { job_id, fingerprint } => {
                handle_eval_cache_pull(self.state, self.writer, job_id, fingerprint).await;
                true
            }
            ClientMessage::EvalCachePush {
                job_id,
                fingerprint,
                size_bytes,
            } => {
                handle_eval_cache_push(
                    self.state,
                    self.writer,
                    eval_cache,
                    job_id,
                    fingerprint,
                    size_bytes,
                )
                .await;
                true
            }
            ClientMessage::EvalCacheChunk {
                job_id,
                data,
                offset,
                is_final,
            } => {
                handle_eval_cache_chunk(self.state, eval_cache, &job_id, data, offset, is_final)
                    .await;
                true
            }
            ClientMessage::EvalCachePushDone {
                job_id: _,
                fingerprint,
                size_bytes,
            } => {
                handle_eval_cache_push_done(self.state, fingerprint, size_bytes).await;
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
        level: gradient_types::proto::EvalMessageLevel,
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
        let registered_peers = match lookup_base_worker_challenge(self.state, self.peer_id).await {
            Some(b) => b.challenge,
            None => lookup_registered_peers(self.state, self.peer_id).await,
        };
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
        let base = lookup_base_worker_challenge(self.state, self.peer_id).await;
        let registered_peers = match &base {
            Some(b) => b.challenge.clone(),
            None => lookup_registered_peers(self.state, self.peer_id).await,
        };
        let (token_authorized, failed_peers) = validate_tokens(&registered_peers, &tokens);
        let authorized_peers = expand_base_authorized(&base, token_authorized);

        // A base worker must never reach PeerAuth::Open (empty == Open). If it has no
        // authorized orgs (toggled off everywhere, or globally disabled), disconnect.
        let is_base =
            gradient_db::base_workers::worker_id_is_base(&self.state.worker_db, self.peer_id)
                .await
                .unwrap_or(false);
        if is_base && authorized_peers.is_empty() {
            info!(peer_id = %self.peer_id, "base worker not enabled by any organization - disconnecting");
            let _ = send_server_msg(
                self.writer,
                &ServerMessage::Reject {
                    code: 403,
                    reason: "base worker not enabled by any organization".into(),
                },
            )
            .await;
            return false;
        }

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
        network_speed_mbps: Option<f32>,
    ) {
        debug!(peer_id = %self.peer_id, cpu_usage_pct, ram_free_mb, ?disk_speed_mbps, ?network_speed_mbps, "WorkerMetrics");
        self.scheduler
            .update_worker_metrics(self.peer_id, cpu_usage_pct, ram_free_mb, disk_speed_mbps, network_speed_mbps)
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
                        gradient_entity::evaluation::EvaluationStatus::Fetching,
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
                        gradient_entity::evaluation::EvaluationStatus::EvaluatingFlake,
                    )
                    .await;
            }
            JobUpdateKind::EvaluatingDerivations => {
                self.scheduler
                    .handle_eval_status_update(
                        &job_id,
                        gradient_entity::evaluation::EvaluationStatus::EvaluatingDerivation,
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
            JobUpdateKind::BuildOutput {
                build_id,
                outputs,
                metrics,
                substituted,
            } => {
                if let Err(e) = self
                    .scheduler
                    .handle_build_output(&job_id, &build_id, outputs, metrics, substituted)
                    .await
                {
                    error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_build_output failed");
                }
            }
            JobUpdateKind::Compressing => {}
            JobUpdateKind::EvalStats(report) => {
                if let Err(e) = self.scheduler.record_eval_metrics(&job_id, report).await {
                    error!(peer_id = %self.peer_id, %job_id, error = %e, "record_eval_metrics failed");
                }
            }
            JobUpdateKind::InputUpdateResult {
                candidate_lock,
                bumped,
            } => {
                self.scheduler
                    .persist_input_update_result(&job_id, candidate_lock, bumped)
                    .await;
            }
        }
    }

    // ── Job terminal states ───────────────────────────────────────────────────

    async fn on_job_completed(&mut self, job_id: String) {
        info!(peer_id = %self.peer_id, %job_id, "job completed");
        if let Err(e) = self
            .scheduler
            .handle_job_completed(self.peer_id, &job_id)
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
        kind: gradient_types::proto::BuildFailureKind,
        missing_paths: Vec<String>,
    ) {
        warn!(peer_id = %self.peer_id, %job_id, %error, ?kind, "job failed");
        if let Err(e) = self
            .scheduler
            .handle_job_failed(self.peer_id, &job_id, &error, kind, &missing_paths)
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
                if let Err(e) = serve_nar_request(&state, &writer, &job_id, &store_path, 0, None).await
                {
                    warn!(%peer_id, %job_id, %store_path, error = %e, "NarRequest serve failed");
                }
            });
        }
    }

    /// Resume a previously-interrupted download from `received_bytes`. Mirrors
    /// [`Self::on_nar_request`]'s per-path spawn, for the single resumed path.
    async fn on_nar_request_resume(
        &mut self,
        job_id: String,
        store_path: String,
        received_bytes: u64,
        stream_token: String,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, received_bytes, "NarRequestResume");
        let state = Arc::clone(self.state);
        let writer = self.writer.clone();
        let permit = Arc::clone(self.nar_serve_semaphore);
        let peer_id = self.peer_id.to_owned();
        tokio::spawn(async move {
            let _guard = match permit.acquire_owned().await {
                Ok(g) => g,
                Err(_) => return,
            };
            if let Err(e) = serve_nar_request(
                &state,
                &writer,
                &job_id,
                &store_path,
                received_bytes,
                Some(&stream_token),
            )
            .await
            {
                warn!(%peer_id, %job_id, %store_path, error = %e, "NarRequestResume serve failed");
            }
        });
    }

    /// Open (or resume) a push stream and tell the worker how many compressed
    /// bytes are already staged so it can seek its regenerated zstd stream.
    async fn on_push_stream_header(
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

    async fn on_nar_push(
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
    /// For S3 / presigned uploads (no preceding push stream), there is nothing
    /// staged to commit - the worker has already PUT the bytes directly to
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

        if nar.is_active(&store_path) {
            let Some(hash) = store_hash(&store_path) else {
                let reason = format!("NarUploaded for malformed store path {store_path}");
                error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "NarUploaded for malformed store path");
                self.abort_job(&job_id, reason).await;
                return;
            };
            let staged = nar.committed_len(&store_path).await;
            if staged != file_size {
                let reason = format!(
                    "staged NAR size {staged} does not match reported file_size {file_size}"
                );
                error!(peer_id = %self.peer_id, %job_id, %store_path, %reason, "NAR upload integrity check failed");
                self.fail_build_transient(&job_id, reason).await;
                return;
            }
            let buf = match nar.read_staged(&store_path).await {
                Ok(b) => b,
                Err(e) => {
                    let reason = format!("failed to read staged NAR: {e}");
                    error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "read staged NAR failed");
                    self.fail_build_transient(&job_id, reason).await;
                    return;
                }
            };
            if let Err(e) = self.state.nar_storage.put(hash, buf).await {
                let reason = format!("failed to write NAR to storage: {e}");
                error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "nar_storage.put failed");
                self.fail_build_transient(&job_id, reason).await;
                return;
            }
            nar.finish(&store_path).await;
            debug!(peer_id = %self.peer_id, %job_id, %store_path, file_size, "NAR stored");
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

    // ── Cache queries ─────────────────────────────────────────────────────────

    async fn on_cache_query(
        &mut self,
        job_id: String,
        paths: Vec<String>,
        mode: gradient_types::proto::QueryMode,
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
                gradient_sources::parse_drv_hash_name(p)
                    .ok()
                    .map(|(h, _)| h)
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let known = match self.scheduler.peer_id_for_job(&job_id).await {
            Some(_) => {
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

                // First: find derivations that exist globally by hash.
                let candidates = EDerivation::find()
                    .filter(CDerivation::Hash.is_in(hashes))
                    .all(&self.state.worker_db)
                    .await
                    .unwrap_or_default();

                if candidates.is_empty() {
                    vec![]
                } else {
                    // Prune a subtree only when every output is on a real upstream
                    // (`external_url`), which serves a complete closure. Our own
                    // cache is output-only and may be missing a node's subtree, so
                    // it is not accepted for pruning - otherwise a config-specific
                    // node off-upstream is pruned, never recorded/built, and dead-ends
                    // its dependents on `InputsUnavailable`.
                    let drv_ids: Vec<DerivationId> = candidates.iter().map(|d| d.id).collect();
                    let outputs = EDerivationOutput::find()
                        .filter(CDerivationOutput::Derivation.is_in(drv_ids))
                        .all(&self.state.worker_db)
                        .await
                        .unwrap_or_default();

                    let candidates: Vec<(DerivationId, String)> =
                        candidates.into_iter().map(|d| (d.id, d.store_path())).collect();
                    prunable_known_derivations(candidates, &outputs)
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

/// Decide which `(derivation_id, store_path)` candidates the eval BFS may prune.
///
/// A derivation is safe to prune (skip subtree traversal) ONLY when every one of
/// its outputs is available on a real upstream cache (`external_url`). An upstream
/// binary cache serves a *complete closure*, so a build worker can fetch the
/// pruned subtree's outputs on demand. Our own cache is deliberately NOT accepted
/// here: it is populated output-only (substitution relays just the output NAR, and
/// a config-specific node's subtree may never have been pushed), so pruning on
/// `is_cached`/`cached_path` strands that subtree - it is never walked, recorded,
/// or built, and being off-upstream it is unfetchable, a permanent
/// `InputsUnavailable` dead-end (observed: `unit-*.service` -> `X-Restart-Triggers-*`
/// / `unit-script-*`, none of which exist on cache.nixos.org). A derivation with
/// any output not on an upstream keeps being walked so its subtree is recorded and
/// scheduled - the eval re-walking our own (unreliable) cached closures is the
/// correctness price of an output-only cache.
fn prunable_known_derivations(
    candidates: Vec<(DerivationId, String)>,
    outputs: &[MDerivationOutput],
) -> Vec<String> {
    let mut counts: HashMap<DerivationId, (usize, usize)> = HashMap::new();
    for o in outputs {
        let entry = counts.entry(o.derivation).or_insert((0, 0));
        entry.0 += 1;
        if o.external_url.is_none() {
            entry.1 += 1;
        }
    }

    candidates
        .into_iter()
        .filter(|(id, _)| {
            let (total, off_upstream) = counts.get(id).copied().unwrap_or((0, 0));
            total > 0 && off_upstream == 0
        })
        .map(|(_, store_path)| store_path)
        .collect()
}

#[cfg(test)]
mod prunable_known_derivations_tests {
    use super::prunable_known_derivations;
    use gradient_types::MDerivationOutput;
    use gradient_types::ids::{DerivationId, DerivationOutputId};

    fn output(drv: DerivationId, hash: &str) -> MDerivationOutput {
        MDerivationOutput {
            id: DerivationOutputId::now_v7(),
            derivation: drv,
            hash: hash.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn prunes_only_outputs_on_a_real_upstream() {
        // Only `external_url` (a real upstream that serves a complete closure) is
        // safe to prune; our own output-only cache (`is_cached` / `cached_path`) is
        // not, because a config-specific node's subtree may never have been pushed.
        let local = DerivationId::now_v7(); // is_cached in our cache, NOT upstream
        let upstream = DerivationId::now_v7(); // every output on an upstream
        let partial = DerivationId::now_v7(); // one output upstream, one not
        let output_less = DerivationId::now_v7(); // recorded drv, no outputs
        let unknown = DerivationId::now_v7(); // no rows at all

        let mut o_local = output(local, "aaa");
        o_local.is_cached = true;
        let mut o_upstream = output(upstream, "bbb");
        o_upstream.external_url = Some("https://cache.example/bbb.narinfo".to_string());
        let mut o_partial_a = output(partial, "ddd");
        o_partial_a.external_url = Some("https://cache.example/ddd.narinfo".to_string());
        let mut o_partial_b = output(partial, "eee");
        o_partial_b.is_cached = true; // in our cache, but not upstream

        let outputs = vec![o_local, o_upstream, o_partial_a, o_partial_b];

        let candidates = vec![
            (local, "/nix/store/aaa-local".to_string()),
            (upstream, "/nix/store/bbb-upstream".to_string()),
            (partial, "/nix/store/ddd-partial".to_string()),
            (output_less, "/nix/store/fff-output-less".to_string()),
            (unknown, "/nix/store/ggg-unknown".to_string()),
        ];

        let prunable = prunable_known_derivations(candidates, &outputs);
        assert_eq!(prunable, vec!["/nix/store/bbb-upstream".to_string()]);
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
        let s =
            NarReceiveStore::new(dir.path().to_path_buf(), "peer-1", Duration::from_secs(3600), max_bytes)
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
