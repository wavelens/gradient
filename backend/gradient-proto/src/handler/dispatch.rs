/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-connection message dispatch context and all `ClientMessage` handlers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_exec::strip_nix_store_prefix;
use gradient_types::ids::ProjectId;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::messages::{
    CACHE_QUERY_BUDGET, CandidateScore, ClientMessage, JobKind, JobPhaseSpan, JobUpdateKind,
    QueryMode, ServerMessage,
};
use gradient_scheduler::Scheduler;
use gradient_scheduler::actor::{WorkerCapabilities, WorkerMetrics};
use gradient_scheduler::jobs::PendingJob;

use super::auth::{
    expand_base_authorized, lookup_base_worker_challenge, lookup_registered_peers, validate_tokens,
};
use super::cache::handle_cache_query;
use super::eval_cache::{
    EvalCacheReceiveStore, handle_eval_cache_chunk, handle_eval_cache_pull, handle_eval_cache_push,
    handle_eval_cache_push_done,
};
use super::nar_transfer::{NarReceiveStore, serve_nar_request};
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoWriter, push_pending_candidates, send_credentials_for_job,
    send_error, send_server_msg,
};

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
    /// Jobs this session currently runs, kept so a core restart can re-register
    /// them without a DB round-trip.
    pub active: &'a mut HashMap<String, PendingJob>,
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
                self.spawn_worker_metrics(
                    cpu_usage_pct,
                    ram_free_mb,
                    disk_speed_mbps,
                    network_speed_mbps,
                );
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
            ClientMessage::JobCompleted { job_id, spans } => {
                self.on_job_completed(job_id, spans).await;
                true
            }
            ClientMessage::JobFailed {
                job_id,
                error,
                kind,
                missing_paths,
                spans,
            } => {
                self.on_job_failed(job_id, error, kind, missing_paths, spans)
                    .await;
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
                ca,
            } => {
                self.on_nar_uploaded(
                    job_id, store_path, file_hash, file_size, nar_size, nar_hash, references,
                    deriver, ca, nar,
                )
                .await;
                true
            }
            ClientMessage::EvalCachePull {
                job_id,
                fingerprint,
            } => {
                self.on_eval_cache_pull(job_id, fingerprint).await;
                true
            }
            ClientMessage::EvalCachePush {
                job_id,
                fingerprint,
                size_bytes,
            } => {
                self.on_eval_cache_push(job_id, fingerprint, size_bytes, eval_cache)
                    .await;
                true
            }
            ClientMessage::EvalCacheChunk {
                job_id,
                data,
                offset,
                is_final,
            } => {
                self.on_eval_cache_chunk(job_id, data, offset, is_final, eval_cache)
                    .await;
                true
            }
            ClientMessage::EvalCachePushDone {
                job_id: _,
                fingerprint,
                size_bytes,
            } => {
                self.on_eval_cache_push_done(fingerprint, size_bytes).await;
                true
            }
            ClientMessage::CacheQuery {
                job_id,
                query_id,
                paths,
                mode,
            } => {
                self.spawn_cache_query(job_id, query_id, paths, mode);
                true
            }
            ClientMessage::QueryKnownDerivations { job_id, drv_paths } => {
                self.spawn_query_known_derivations(job_id, drv_paths);
                true
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

    /// Snapshot the owned handles needed to run an order-independent RPC off the
    /// dispatch loop, so a slow handler can't head-of-line-block cache lookups.
    fn rpc(&self) -> RpcContext {
        RpcContext {
            state: Arc::clone(self.state),
            scheduler: Arc::clone(self.scheduler),
            writer: self.writer.clone(),
            peer_id: self.peer_id.to_owned(),
        }
    }

    // ── Order-independent RPCs (run off the dispatch loop) ────────────────────

    fn spawn_worker_metrics(
        &self,
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
        network_speed_mbps: Option<f32>,
    ) {
        let rpc = self.rpc();
        self.state.shutdown.spawn(async move {
            rpc.on_worker_metrics(
                cpu_usage_pct,
                ram_free_mb,
                disk_speed_mbps,
                network_speed_mbps,
            )
            .await;
        });
    }

    fn spawn_cache_query(
        &self,
        job_id: String,
        query_id: String,
        paths: Vec<String>,
        mode: QueryMode,
    ) {
        let rpc = self.rpc();
        self.state
            .shutdown
            .spawn(async move { rpc.on_cache_query(job_id, query_id, paths, mode).await });
    }

    fn spawn_query_known_derivations(&self, job_id: String, drv_paths: Vec<String>) {
        let rpc = self.rpc();
        self.state
            .shutdown
            .spawn(async move { rpc.on_query_known_derivations(job_id, drv_paths).await });
    }

    // ── Eval cache ────────────────────────────────────────────────────────────

    async fn on_eval_cache_pull(&mut self, job_id: String, fingerprint: String) {
        handle_eval_cache_pull(self.state, self.writer, job_id, fingerprint).await;
    }

    async fn on_eval_cache_push(
        &mut self,
        job_id: String,
        fingerprint: String,
        size_bytes: u64,
        eval_cache: &mut EvalCacheReceiveStore,
    ) {
        handle_eval_cache_push(
            self.state,
            self.writer,
            eval_cache,
            job_id,
            fingerprint,
            size_bytes,
        )
        .await;
    }

    async fn on_eval_cache_chunk(
        &mut self,
        job_id: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
        eval_cache: &mut EvalCacheReceiveStore,
    ) {
        handle_eval_cache_chunk(self.state, eval_cache, &job_id, data, offset, is_final).await;
    }

    async fn on_eval_cache_push_done(&mut self, fingerprint: String, size_bytes: u64) {
        handle_eval_cache_push_done(self.state, fingerprint, size_bytes).await;
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
        // authorized projects (toggled off everywhere, or globally disabled), disconnect.
        let is_base =
            gradient_db::base_workers::worker_id_is_base(&self.state.worker_db, self.peer_id)
                .await
                .unwrap_or(false);
        if is_base && authorized_peers.is_empty() {
            info!(peer_id = %self.peer_id, "base worker not enabled by any project - disconnecting");
            let _ = send_server_msg(
                self.writer,
                &ServerMessage::Reject {
                    code: 403,
                    reason: "base worker not enabled by any project".into(),
                },
            )
            .await;
            return false;
        }

        let updated_uuids: HashSet<ProjectId> = authorized_peers
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

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WorkerCapabilities wire fields; refactor tracked in #503"
    )]
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
                WorkerCapabilities {
                    architectures,
                    system_features,
                    max_concurrent_builds,
                    cpu_count,
                    ram_total_mb,
                    cpu_core_score,
                },
            )
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
            self.active
                .insert(assignment.job_id.clone(), assignment.pending.clone());
            send_credentials_for_job(
                self.writer,
                self.state,
                self.scheduler,
                self.peer_id,
                &assignment.job,
                assignment.project_id,
            )
            .await;
            if send_server_msg(
                self.writer,
                &ServerMessage::AssignJob {
                    job_id: assignment.job_id,
                    job: assignment.job,
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
            self.active.remove(&job_id);
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
            JobUpdateKind::InputUpdateExpansion { matched } => {
                self.scheduler
                    .persist_input_update_expansion(&job_id, matched)
                    .await;
            }
        }
    }

    // ── Job terminal states ───────────────────────────────────────────────────

    async fn on_job_completed(&mut self, job_id: String, spans: Vec<JobPhaseSpan>) {
        info!(peer_id = %self.peer_id, %job_id, phases = spans.len(), "job completed");
        let _ = spans;
        self.active.remove(&job_id);
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
        spans: Vec<JobPhaseSpan>,
    ) {
        warn!(peer_id = %self.peer_id, %job_id, %error, ?kind, phases = spans.len(), "job failed");
        let _ = spans;
        self.active.remove(&job_id);
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
        let shutdown = self.state.shutdown.clone();
        for store_path in paths {
            let state = Arc::clone(self.state);
            let writer = self.writer.clone();
            let permit = Arc::clone(self.nar_serve_semaphore);
            let peer_id = self.peer_id.to_owned();
            let job_id = job_id.clone();
            shutdown.spawn(async move {
                let _guard = match permit.acquire_owned().await {
                    Ok(g) => g,
                    Err(_) => return, // semaphore closed (shutdown)
                };
                if let Err(e) =
                    serve_nar_request(&state, &writer, &job_id, &store_path, 0, None).await
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
        let shutdown = self.state.shutdown.clone();
        shutdown.spawn(async move {
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
}

/// Owned handles for the order-independent request/response RPCs, spawned off
/// the per-connection dispatch loop so a slow upstream probe or NAR transfer
/// can't head-of-line-block a worker's `CacheQuery` (its 120s `CacheStatus`
/// deadline). Replies travel the cloneable writer, so out-of-order completion
/// is safe.
pub(super) struct RpcContext {
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    writer: ProtoWriter,
    peer_id: String,
}

impl RpcContext {
    async fn on_cache_query(
        &self,
        job_id: String,
        query_id: String,
        paths: Vec<String>,
        mode: gradient_types::proto::QueryMode,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %query_id, count = paths.len(), ?mode, "CacheQuery");
        let project_id = self.scheduler.project_for_job(&job_id).await;

        // A DB error or an over-budget handler is *indeterminate*, never
        // "absent": reply `CacheError` so the worker retries transiently instead
        // of taking a fully-cached input as a missing one (terminal
        // `InputsUnavailable`, which fails the whole eval).
        let reply = match tokio::time::timeout(
            CACHE_QUERY_BUDGET,
            handle_cache_query(&self.state, project_id, &paths, mode),
        )
        .await
        {
            Ok(Ok(cached)) => {
                debug!(peer_id = %self.peer_id, %job_id, %query_id, entries = cached.len(), "CacheStatus");
                ServerMessage::CacheStatus { query_id, cached }
            }
            Ok(Err(e)) => {
                warn!(peer_id = %self.peer_id, %job_id, %query_id, error = %e, "CacheQuery DB error; replying CacheError");
                ServerMessage::CacheError {
                    query_id,
                    message: format!("cache lookup failed: {e}"),
                }
            }
            Err(_) => {
                warn!(peer_id = %self.peer_id, %job_id, %query_id, budget_secs = CACHE_QUERY_BUDGET.as_secs(), "CacheQuery exceeded server budget; replying CacheError");
                ServerMessage::CacheError {
                    query_id,
                    message: "cache query exceeded server budget".to_string(),
                }
            }
        };

        if send_server_msg(&self.writer, &reply).await.is_err() {
            debug!(peer_id = %self.peer_id, "CacheStatus/CacheError send failed; connection closing");
        }
    }

    async fn on_query_known_derivations(&self, job_id: String, drv_paths: Vec<String>) {
        debug!(peer_id = %self.peer_id, %job_id, count = drv_paths.len(), "QueryKnownDerivations");
        // Our own cache is output-only, so only `external_url` upstreams (which
        // serve a complete closure) gate pruning - see `gradient_graph::known`.
        let hashes: Vec<String> = drv_paths
            .iter()
            .map(|p| strip_nix_store_prefix(p))
            .filter_map(|p| {
                gradient_sources::parse_drv_hash_name(&p)
                    .ok()
                    .map(|(h, _)| h)
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let known = match self.scheduler.project_for_job(&job_id).await {
            Some(_) => match self.state.graph.known_derivations(hashes).await {
                Ok(known) => known,
                Err(e) => {
                    warn!(peer_id = %self.peer_id, %job_id, error = %e, "QueryKnownDerivations degraded; pruning nothing");
                    vec![]
                }
            },
            None => {
                warn!(peer_id = %self.peer_id, %job_id, "QueryKnownDerivations: no project for job");
                vec![]
            }
        };
        debug!(peer_id = %self.peer_id, %job_id, known = known.len(), "KnownDerivations");
        if send_server_msg(
            &self.writer,
            &ServerMessage::KnownDerivations { job_id, known },
        )
        .await
        .is_err()
        {
            debug!(peer_id = %self.peer_id, "KnownDerivations send failed; connection closing");
        }
    }

    async fn on_worker_metrics(
        &self,
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
        network_speed_mbps: Option<f32>,
    ) {
        debug!(peer_id = %self.peer_id, cpu_usage_pct, ram_free_mb, ?disk_speed_mbps, ?network_speed_mbps, "WorkerMetrics");
        self.scheduler
            .update_worker_metrics(
                &self.peer_id,
                WorkerMetrics {
                    cpu_usage_pct,
                    ram_free_mb,
                    disk_speed_mbps,
                    network_speed_mbps,
                },
            )
            .await;
    }
}
