/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-connection message dispatch context and all `ClientMessage` handlers.

use std::collections::HashSet;
use std::sync::Arc;

use gradient_core::types::*;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::messages::{CandidateScore, ClientMessage, JobKind, JobUpdateKind, ServerMessage};
use scheduler::Scheduler;

use super::auth::{lookup_registered_peers, validate_tokens};
use super::cache::handle_cache_query;
use super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoSocket, push_pending_candidates, send_credentials_for_job,
    send_error, send_server_msg, serve_nar_request,
};

// ── Dispatch context ──────────────────────────────────────────────────────────

/// Holds the per-connection references needed to handle a single client message.
pub(super) struct DispatchContext<'a> {
    pub socket: &'a mut ProtoSocket,
    pub state: &'a Arc<ServerState>,
    pub scheduler: &'a Arc<Scheduler>,
    pub peer_id: &'a str,
}

impl<'a> DispatchContext<'a> {
    /// Route a single `ClientMessage` to the appropriate handler.
    ///
    /// Returns `true` to continue the loop, `false` to break.
    pub async fn dispatch(
        &mut self,
        msg: ClientMessage,
        nar_buffers: &mut std::collections::HashMap<String, Vec<u8>>,
    ) -> bool {
        debug!(?msg, "received client message");
        match msg {
            ClientMessage::InitConnection { .. } => {
                send_error(self.socket, 400, "unexpected InitConnection".into()).await;
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
            } => {
                self.on_worker_capabilities(architectures, system_features, max_concurrent_builds)
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
            ClientMessage::JobFailed { job_id, error } => {
                self.on_job_failed(job_id, error).await;
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
            } => {
                self.on_nar_uploaded(
                    job_id, store_path, file_hash, file_size, nar_size, nar_hash, references,
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
        }
    }

    // ── Reauth ────────────────────────────────────────────────────────────────

    async fn on_reauth_request(&mut self) -> bool {
        debug!(peer_id = %self.peer_id, "ReauthRequest");
        let registered_peers = lookup_registered_peers(self.state, self.peer_id).await;
        send_server_msg(
            self.socket,
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
        let updated_uuids: HashSet<Uuid> = authorized_peers
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        self.scheduler
            .update_authorized_peers(self.peer_id, updated_uuids)
            .await;
        send_server_msg(
            self.socket,
            &ServerMessage::AuthUpdate {
                authorized_peers,
                failed_peers,
            },
        )
        .await
        .is_ok()
    }

    // ── Capability advertisement ──────────────────────────────────────────────

    async fn on_worker_capabilities(
        &mut self,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
    ) {
        debug!(peer_id = %self.peer_id, ?architectures, ?system_features, max_concurrent_builds, "WorkerCapabilities");
        self.scheduler
            .update_worker_capabilities(
                self.peer_id,
                architectures,
                system_features,
                max_concurrent_builds,
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
                self.socket,
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
                self.socket,
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
                self.socket,
                self.state,
                self.scheduler,
                self.peer_id,
                &assignment.job,
                assignment.peer_id,
            )
            .await;
            if send_server_msg(
                self.socket,
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
                push_pending_candidates(self.socket, self.scheduler, self.peer_id).await;
            }
            JobUpdateKind::Building { build_id } => {
                self.scheduler.handle_build_status_update(&build_id).await;
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

    async fn on_job_completed(&mut self, job_id: String) {
        info!(peer_id = %self.peer_id, %job_id, "job completed");
        if let Err(e) = self
            .scheduler
            .handle_job_completed(self.peer_id, &job_id)
            .await
        {
            error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_job_completed failed");
        }
        push_pending_candidates(self.socket, self.scheduler, self.peer_id).await;
    }

    async fn on_job_failed(&mut self, job_id: String, error: String) {
        warn!(peer_id = %self.peer_id, %job_id, %error, "job failed");
        if let Err(e) = self
            .scheduler
            .handle_job_failed(self.peer_id, &job_id, &error)
            .await
        {
            error!(peer_id = %self.peer_id, %job_id, error = %e, "handle_job_failed failed");
        }
        push_pending_candidates(self.socket, self.scheduler, self.peer_id).await;
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
        for store_path in paths {
            if let Err(e) = serve_nar_request(self.state, self.socket, &job_id, &store_path).await {
                warn!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "NarRequest serve failed");
            }
        }
    }

    async fn on_nar_push(
        &mut self,
        job_id: String,
        store_path: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
        nar_buffers: &mut std::collections::HashMap<String, Vec<u8>>,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, offset, is_final, bytes = data.len(), "NarPush");
        if !data.is_empty() {
            nar_buffers
                .entry(store_path.clone())
                .or_default()
                .extend_from_slice(&data);
        }
        if !is_final {
            return;
        }
        let buf = nar_buffers.remove(&store_path).unwrap_or_default();
        let compressed_size = buf.len() as i64;
        let hash_opt = store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(&store_path)
            .split('-')
            .next()
            .map(str::to_owned);
        match hash_opt {
            Some(hash) => {
                if let Err(e) = self.state.nar_storage.put(&hash, buf).await {
                    error!(peer_id = %self.peer_id, %job_id, %store_path, error = %e, "NarPush write failed");
                } else {
                    info!(peer_id = %self.peer_id, %job_id, %store_path, compressed_size, "NarPush stored");
                }
            }
            None => {
                warn!(peer_id = %self.peer_id, %job_id, %store_path, "NarPush: could not parse store path hash")
            }
        }
    }

    async fn on_nar_uploaded(
        &mut self,
        job_id: String,
        store_path: String,
        file_hash: String,
        file_size: u64,
        nar_size: u64,
        nar_hash: String,
        references: Vec<String>,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %store_path, %file_hash, file_size, nar_size, %nar_hash, "NarUploaded");
        let file_size_i64 = file_size as i64;
        let nar_record = NarUploadRecord {
            file_hash: &file_hash,
            file_size: file_size_i64,
            nar_size: nar_size as i64,
            nar_hash: &nar_hash,
            references: &references,
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
        send_server_msg(self.socket, &ServerMessage::CacheStatus { job_id, cached })
            .await
            .is_ok()
    }

    async fn on_query_known_derivations(
        &mut self,
        job_id: String,
        drv_paths: Vec<String>,
    ) -> bool {
        debug!(peer_id = %self.peer_id, %job_id, count = drv_paths.len(), "QueryKnownDerivations");
        let known = match self.scheduler.peer_id_for_job(&job_id).await {
            Some(org_id) => {
                use entity::build::BuildStatus;
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

                // First: find derivations that exist for this org.
                let candidates = EDerivation::find()
                    .filter(CDerivation::Organization.eq(org_id))
                    .filter(CDerivation::DerivationPath.is_in(drv_paths))
                    .all(&self.state.db)
                    .await
                    .unwrap_or_default();

                if candidates.is_empty() {
                    vec![]
                } else {
                    // Second: keep only those that have a Completed or Substituted build.
                    // A derivation exists in the DB but has only Failed builds should
                    // NOT be pruned — the worker must retry it.
                    let drv_ids: Vec<Uuid> = candidates.iter().map(|d| d.id).collect();
                    let built: std::collections::HashSet<Uuid> = EBuild::find()
                        .filter(CBuild::Derivation.is_in(drv_ids))
                        .filter(CBuild::Status.is_in(vec![
                            BuildStatus::Completed,
                            BuildStatus::Substituted,
                        ]))
                        .all(&self.state.db)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|b| b.derivation)
                        .collect();

                    candidates
                        .into_iter()
                        .filter(|d| built.contains(&d.id))
                        .map(|d| d.derivation_path)
                        .collect()
                }
            }
            None => {
                warn!(peer_id = %self.peer_id, %job_id, "QueryKnownDerivations: no org for job");
                vec![]
            }
        };
        debug!(peer_id = %self.peer_id, %job_id, known = known.len(), "KnownDerivations");
        send_server_msg(self.socket, &ServerMessage::KnownDerivations { job_id, known })
            .await
            .is_ok()
    }
}
