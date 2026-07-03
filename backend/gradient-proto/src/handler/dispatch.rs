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
use gradient_types::ids::{DerivationId, OrganizationId};
use gradient_types::*;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::messages::{
    CACHE_QUERY_BUDGET, CandidateScore, ClientMessage, JobKind, JobUpdateKind, QueryMode,
    ServerMessage,
};
use gradient_scheduler::Scheduler;

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
    /// Bounds the detached `NarUploaded` commits per connection - each pins a
    /// whole staged NAR in memory while writing it to `nar_storage`.
    pub nar_commit_semaphore: &'a Arc<Semaphore>,
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
                    job_id, store_path, file_hash, file_size, nar_size, nar_hash, references,
                    deriver, nar,
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
                paths,
                mode,
            } => {
                self.spawn_cache_query(job_id, paths, mode);
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
        tokio::spawn(async move {
            rpc.on_worker_metrics(
                cpu_usage_pct,
                ram_free_mb,
                disk_speed_mbps,
                network_speed_mbps,
            )
            .await;
        });
    }

    fn spawn_cache_query(&self, job_id: String, paths: Vec<String>, mode: QueryMode) {
        let rpc = self.rpc();
        tokio::spawn(async move { rpc.on_cache_query(job_id, paths, mode).await });
    }

    fn spawn_query_known_derivations(&self, job_id: String, drv_paths: Vec<String>) {
        let rpc = self.rpc();
        tokio::spawn(async move { rpc.on_query_known_derivations(job_id, drv_paths).await });
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
                assignment.org_id,
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
        paths: Vec<String>,
        mode: gradient_types::proto::QueryMode,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, count = paths.len(), ?mode, "CacheQuery");
        let org_id = self.scheduler.org_for_job(&job_id).await;

        // A DB error or an over-budget handler is *indeterminate*, never
        // "absent": reply `CacheError` so the worker retries transiently instead
        // of taking a fully-cached input as a missing one (terminal
        // `InputsUnavailable`, which fails the whole eval).
        let reply = match tokio::time::timeout(
            CACHE_QUERY_BUDGET,
            handle_cache_query(&self.state, org_id, &paths, mode),
        )
        .await
        {
            Ok(Ok(cached)) => {
                debug!(peer_id = %self.peer_id, %job_id, entries = cached.len(), "CacheStatus");
                ServerMessage::CacheStatus { job_id, cached }
            }
            Ok(Err(e)) => {
                warn!(peer_id = %self.peer_id, %job_id, error = %e, "CacheQuery DB error; replying CacheError");
                ServerMessage::CacheError {
                    job_id,
                    message: format!("cache lookup failed: {e}"),
                }
            }
            Err(_) => {
                warn!(peer_id = %self.peer_id, %job_id, budget_secs = CACHE_QUERY_BUDGET.as_secs(), "CacheQuery exceeded server budget; replying CacheError");
                ServerMessage::CacheError {
                    job_id,
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
        // serve a complete closure) gate pruning - see `prunable_known_derivations`.
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
        let known = match self.scheduler.org_for_job(&job_id).await {
            // A degraded DB read must prune NOTHING (the worker re-walks, which
            // is always safe), never masquerade as "no rows": an empty
            // `edges_unresolved` set from a failed query would re-prune an
            // anchor whose dropped edge only a re-walk can restore.
            Some(_) => match self.load_prunable(hashes).await {
                Ok(known) => known,
                Err(e) => {
                    warn!(peer_id = %self.peer_id, %job_id, error = %e, "QueryKnownDerivations degraded; pruning nothing");
                    vec![]
                }
            },
            None => {
                warn!(peer_id = %self.peer_id, %job_id, "QueryKnownDerivations: no org for job");
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

    /// The prunable-derivations lookup, on the isolated cache-query pool so an
    /// eval's prefetch storm cannot exhaust the scheduler pool through this
    /// probe. Any error propagates - the caller prunes nothing.
    async fn load_prunable(&self, hashes: Vec<String>) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let db = &self.state.cache_db;
        let candidates = EDerivation::find()
            .filter(CDerivation::Hash.is_in(hashes))
            .all(db)
            .await?;
        if candidates.is_empty() {
            return Ok(vec![]);
        }

        let drv_ids: Vec<DerivationId> = candidates.iter().map(|d| d.id).collect();
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(drv_ids.clone()))
            .all(db)
            .await?;

        let anchors = EDerivationBuild::find()
            .filter(CDerivationBuild::Derivation.is_in(drv_ids))
            .all(db)
            .await?;
        // Anchors carrying a dropped dependency edge (`edges_unresolved`)
        // must be re-walked, not pruned, so the eval rediscovers the edge
        // and clears the flag - otherwise they stay stranded off promotion.
        let unresolved: HashSet<DerivationId> = anchors
            .iter()
            .filter(|b| b.edges_unresolved)
            .map(|b| b.derivation)
            .collect();
        // Local-prune arm precondition: the anchor already succeeded and its
        // subtree's edges are durably recorded, so skipping the walk loses
        // nothing the graph needs.
        let complete_anchors: HashSet<DerivationId> = anchors
            .iter()
            .filter(|b| b.edges_complete && b.status.is_terminal_success())
            .map(|b| b.derivation)
            .collect();

        let out_hashes: Vec<String> = outputs
            .iter()
            .map(|o| o.hash.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let closure_cached: HashSet<String> = ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(out_hashes))
            .all(db)
            .await?
            .into_iter()
            .filter(|cp| cp.is_fully_cached() && cp.closure_complete)
            .map(|cp| cp.hash)
            .collect();

        let candidates: Vec<(DerivationId, String)> = candidates
            .into_iter()
            .map(|d| (d.id, d.store_path()))
            .collect();
        Ok(prunable_known_derivations(
            candidates,
            &outputs,
            &unresolved,
            &complete_anchors,
            &closure_cached,
        ))
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
                cpu_usage_pct,
                ram_free_mb,
                disk_speed_mbps,
                network_speed_mbps,
            )
            .await;
    }
}

/// Decide which `(derivation_id, store_path)` candidates the eval BFS may prune.
///
/// A derivation is safe to prune (skip subtree traversal) when a dependent can
/// fetch everything it needs without the subtree ever being re-walked. Two arms:
///
/// Upstream arm: every output is available on a real upstream cache
/// (`external_url`). An upstream binary cache serves a *complete closure*, so a
/// build worker can fetch the pruned subtree's outputs on demand.
///
/// Local arm: the anchor is terminal-success with `edges_complete` (the eval
/// that produced it durably recorded its subtree's graph) AND every output has
/// a fully-cached `cached_path` with `closure_complete` - the output's full
/// runtime closure is in our cache, so dependents fetch from us. Bare
/// `is_cached`/`cached_path` presence is NOT enough (our cache is populated
/// output-only; pruning on it stranded never-pushed closure members like
/// `unit-*.service` -> `X-Restart-Triggers-*` as permanent `InputsUnavailable`
/// dead-ends); `cached_path.closure_complete` is the reconciled ground truth
/// that closed that hole. If GC later evicts a member, the demote paths clear
/// the flag and the next eval re-walks.
///
/// Without the local arm every built (non-upstream) node - the whole
/// config-specific tree - was re-walked on every evaluation.
fn prunable_known_derivations(
    candidates: Vec<(DerivationId, String)>,
    outputs: &[MDerivationOutput],
    unresolved: &HashSet<DerivationId>,
    complete_anchors: &HashSet<DerivationId>,
    closure_cached: &HashSet<String>,
) -> Vec<String> {
    let mut counts: HashMap<DerivationId, (usize, usize, usize)> = HashMap::new();
    for o in outputs {
        let entry = counts.entry(o.derivation).or_insert((0, 0, 0));
        entry.0 += 1;
        if o.external_url.is_none() {
            entry.1 += 1;
        }
        if !closure_cached.contains(&o.hash) {
            entry.2 += 1;
        }
    }

    // A stale `edges_unresolved` anchor must be re-walked, never pruned: pruning
    // skips the walk that would rediscover its dropped dependency edge (e.g. a dep
    // GC'd out from under it) and clear the flag, leaving it and its dependents
    // stranded off promotion forever.
    candidates
        .into_iter()
        .filter(|(id, _)| {
            let (total, off_upstream, off_local) = counts.get(id).copied().unwrap_or((0, 0, 0));
            let upstream_ok = off_upstream == 0;
            let local_ok = off_local == 0 && complete_anchors.contains(id);
            total > 0 && !unresolved.contains(id) && (upstream_ok || local_ok)
        })
        .map(|(_, store_path)| store_path)
        .collect()
}

#[cfg(test)]
mod prunable_known_derivations_tests {
    use super::prunable_known_derivations;
    use gradient_types::MDerivationOutput;
    use gradient_types::ids::{DerivationId, DerivationOutputId};
    use std::collections::HashSet;

    fn output(drv: DerivationId, hash: &str) -> MDerivationOutput {
        MDerivationOutput {
            id: DerivationOutputId::now_v7(),
            derivation: drv,
            hash: hash.to_string(),
            ..Default::default()
        }
    }

    fn prune(
        candidates: Vec<(DerivationId, String)>,
        outputs: &[MDerivationOutput],
        unresolved: &HashSet<DerivationId>,
        complete_anchors: &HashSet<DerivationId>,
        closure_cached: &HashSet<String>,
    ) -> Vec<String> {
        prunable_known_derivations(candidates, outputs, unresolved, complete_anchors, closure_cached)
    }

    #[test]
    fn prunes_only_outputs_on_a_real_upstream() {
        // `external_url` (a real upstream that serves a complete closure) is safe
        // to prune; bare `is_cached` in our own output-only cache is not, because
        // a config-specific node's subtree may never have been pushed.
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

        let prunable = prune(candidates, &outputs, &HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_eq!(prunable, vec!["/nix/store/bbb-upstream".to_string()]);
    }

    /// The local arm: a terminal-success anchor with recorded edges whose every
    /// output is fully cached with `cached_path.closure_complete` prunes even
    /// off-upstream - without it the whole built (config-specific) tree was
    /// re-walked on every evaluation. Both preconditions are load-bearing: a
    /// closure-complete output without the recorded-graph anchor, or a complete
    /// anchor with one output lacking closure_complete, must keep walking.
    #[test]
    fn locally_closure_complete_anchor_prunes() {
        let complete = DerivationId::now_v7(); // anchor complete + output closure-cached
        let no_anchor = DerivationId::now_v7(); // output closure-cached, no complete anchor
        let half_cached = DerivationId::now_v7(); // anchor complete, one output not closure-cached

        let outputs = vec![
            output(complete, "aaa"),
            output(no_anchor, "bbb"),
            output(half_cached, "ccc"),
            output(half_cached, "ddd"),
        ];
        let candidates = vec![
            (complete, "/nix/store/aaa-complete".to_string()),
            (no_anchor, "/nix/store/bbb-no-anchor".to_string()),
            (half_cached, "/nix/store/ccc-half".to_string()),
        ];
        let complete_anchors = HashSet::from([complete, half_cached]);
        let closure_cached: HashSet<String> =
            ["aaa", "bbb", "ccc"].iter().map(|s| s.to_string()).collect();

        let prunable = prune(candidates, &outputs, &HashSet::new(), &complete_anchors, &closure_cached);
        assert_eq!(prunable, vec!["/nix/store/aaa-complete".to_string()]);
    }

    /// A derivation flagged `edges_unresolved` (a build-dep edge a prior eval could
    /// not record - e.g. the dep was GC'd out from under a shared closure) must NOT
    /// be pruned even when every output is on an upstream or closure-complete
    /// locally: pruning skips the re-walk that rediscovers the dropped edge and
    /// clears the flag, so the anchor and its dependents stay stranded off
    /// promotion forever.
    #[test]
    fn edges_unresolved_anchor_is_never_prunable() {
        let upstream = DerivationId::now_v7();
        let mut o = output(upstream, "bbb");
        o.external_url = Some("https://cache.example/bbb.narinfo".to_string());
        let candidates = vec![(upstream, "/nix/store/bbb-upstream".to_string())];
        let unresolved = HashSet::from([upstream]);
        let complete_anchors = HashSet::from([upstream]);
        let closure_cached: HashSet<String> = HashSet::from(["bbb".to_string()]);

        assert!(
            prune(candidates, &[o], &unresolved, &complete_anchors, &closure_cached).is_empty(),
            "an edges_unresolved anchor must be re-walked, not pruned"
        );
    }
}
