/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use gradient_core::ServerState;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_types::events::{build, evaluation};
use gradient_types::ids::{DerivationBuildId, DispatchedJobId, EvaluationId, ProjectId, TaskId};
use gradient_types::{BuildProgress, BuildProgressPhase, EvaluationProgress};
use gradient_util::store_path::strip_nix_store_prefix;
use tokio::sync::Semaphore;
use tracing::{Instrument as _, debug, debug_span, info, trace, warn};

use gradient_scheduler::actor::{WorkerCapabilities, WorkerMetrics};
use gradient_scheduler::jobs::{Assignment, PendingJob};
use gradient_scheduler::{ReportedTimeline, Scheduler};
use gradient_wire::messages::{
    CACHE_QUERY_BUDGET, CandidateScore, ClientMessage, ClusterMembership, JobKind, ServerMessage,
};
use gradient_wire::types::{
    BuildProgressPhase as WireBuildProgressPhase, EvalProgress as WireEvalProgress,
};

use super::auth::{challenge_for, resolve_authorized};
use super::cache::handle_cache_query;
use super::dialed::{DialedSession, refresh_dialed_peers};
use super::eval_cache::handle_eval_cache_pull;
use super::job_events::{JobEvent, JobEvents};
use super::log_lane::LogLane;
use super::nar_serve::{ServeSlot, serve_nar_request};
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoWriter, send_credentials_for_job, send_error, send_server_msg,
};
use super::upload::UploadSession;

#[derive(Clone)]
pub(crate) struct ActiveJob {
    pub assignment_id: DispatchedJobId,
    pub pending: PendingJob,
    pub cluster: Option<gradient_types::ids::ClusterAttemptId>,
}

#[derive(Clone, Default)]
pub(crate) struct ActiveJobs(Arc<gradient_util::sync::Mutex<HashMap<String, ActiveJob>>>);

impl ActiveJobs {
    pub(crate) fn insert(&self, job_id: String, job: ActiveJob) {
        self.0.lock().insert(job_id, job);
    }

    pub(crate) fn remove(&self, job_id: &str) -> Option<ActiveJob> {
        self.0.lock().remove(job_id)
    }

    pub(crate) fn contains(&self, job_id: &str) -> bool {
        self.0.lock().contains_key(job_id)
    }

    pub(crate) fn assignment_id(&self, job_id: &str) -> Option<DispatchedJobId> {
        self.0.lock().get(job_id).map(|a| a.assignment_id)
    }

    pub(crate) fn project(&self, job_id: &str) -> Option<ProjectId> {
        self.0.lock().get(job_id).map(|a| a.pending.project_id())
    }

    pub(crate) fn build(&self, job_id: &str, task_index: u32) -> Option<DerivationBuildId> {
        match &self.0.lock().get(job_id)?.pending {
            PendingJob::Build(j) => j.job.builds.get(task_index as usize)?.build_id.parse().ok(),
            PendingJob::Eval(_) => None,
        }
    }

    pub(crate) fn eval(&self, job_id: &str) -> Option<(EvaluationId, Option<TaskId>)> {
        match &self.0.lock().get(job_id)?.pending {
            PendingJob::Eval(j) => Some((j.evaluation_id, j.task_id)),
            PendingJob::Build(_) => None,
        }
    }

    pub(crate) fn remove_attempt(
        &self,
        attempt: gradient_types::ids::ClusterAttemptId,
    ) -> Vec<String> {
        let mut jobs = self.0.lock();
        let members: Vec<String> = jobs
            .iter()
            .filter(|(_, job)| job.cluster == Some(attempt))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &members {
            jobs.remove(id);
        }
        members
    }

    pub(crate) fn len(&self) -> usize {
        self.0.lock().len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.lock().is_empty()
    }

    pub(crate) fn pending(&self) -> Vec<gradient_scheduler::jobs::Reattached> {
        self.0
            .lock()
            .iter()
            .map(|(id, job)| gradient_scheduler::jobs::Reattached {
                job_id: id.clone(),
                job: job.pending.clone(),
                cluster: job.cluster,
            })
            .collect()
    }
}

impl From<HashMap<String, ActiveJob>> for ActiveJobs {
    fn from(jobs: HashMap<String, ActiveJob>) -> Self {
        Self(Arc::new(gradient_util::sync::Mutex::new(jobs)))
    }
}

/// Only the dispatch this session handed out is matching, which is covering a reconnecting worker
/// declared dead. One hole is still open. An evicted worker with a live socket can report after a
/// re-dispatch. Closing it is requiring the `DispatchedJobId` check inside the core actor.
pub(super) fn assignment_matches(current: Option<DispatchedJobId>, reported: &str) -> bool {
    match (current, reported.parse::<DispatchedJobId>()) {
        (Some(current), Ok(reported)) => current == reported,
        _ => false,
    }
}

pub(super) struct InboundContext<'a> {
    pub writer: &'a ProtoWriter,
    pub state: &'a Arc<ServerState>,
    pub scheduler: &'a Arc<Scheduler>,
    pub peer_id: &'a str,
    pub nar_serve_semaphore: &'a Arc<Semaphore>,
    pub active: &'a ActiveJobs,
    pub job_events: &'a JobEvents,
    pub logs: &'a LogLane,
    pub dialed: Option<&'a DialedSession>,
}

impl<'a> InboundContext<'a> {
    pub async fn handle(&mut self, msg: ClientMessage, uploads: &mut UploadSession) -> bool {
        trace!(variant = msg.variant_name(), "received client message");
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
                zone,
                endpoint,
            } => {
                self.on_worker_capabilities(WorkerCapabilities {
                    architectures,
                    system_features,
                    max_concurrent_builds,
                    cpu_count,
                    ram_total_mb,
                    cpu_core_score,
                    zone,
                    endpoint,
                })
                .await;
                true
            }
            ClientMessage::WorkerMetrics {
                cpu_usage_pct,
                ram_free_mb,
                disk_speed_mbps,
                upload_speed_mbps,
                download_speed_mbps,
            } => {
                self.spawn_worker_metrics(WorkerMetrics {
                    cpu_usage_pct,
                    ram_free_mb,
                    disk_speed_mbps,
                    upload_speed_mbps,
                    download_speed_mbps,
                });
                true
            }
            ClientMessage::RequestJobList => self.on_request_job_list().await,
            ClientMessage::RequestJob { kind } => self.on_request_job(kind).await,
            ClientMessage::ClusterSignal {
                attempt,
                to,
                payload,
            } => {
                self.scheduler
                    .forward_cluster_signal(self.peer_id, &attempt, to, payload.into())
                    .await;
                true
            }
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
            ClientMessage::JobUpdate {
                job_id,
                assignment_id,
                update,
            } => {
                if self.owns(&job_id, &assignment_id) {
                    debug!(peer_id = %self.peer_id, %job_id, ?update, "JobUpdate");
                    self.job_events
                        .push(JobEvent::Update { job_id, update })
                        .await;
                }
                true
            }
            ClientMessage::BuildProgress {
                job_id,
                assignment_id,
                build_id,
                phase,
                bytes_done,
                bytes_total,
                paths_done,
                paths_total,
            } => {
                if self.owns(&job_id, &assignment_id) {
                    let progress = BuildProgress {
                        phase: build_progress_phase(phase),
                        bytes_done,
                        bytes_total,
                        paths_done,
                        paths_total,
                    };
                    self.on_build_progress(&build_id, progress);
                }
                true
            }
            ClientMessage::EvalProgress {
                job_id,
                assignment_id,
                progress,
            } => {
                if self.owns(&job_id, &assignment_id) {
                    self.on_eval_progress(&job_id, progress);
                }
                true
            }
            ClientMessage::JobCompleted {
                job_id,
                assignment_id,
                spans,
                elapsed_ms,
            } => {
                let report = ReportedTimeline::received(spans, elapsed_ms);
                self.forget_uploads(&job_id, uploads).await;
                self.logs.flush().await;
                if let Some(assignment_id) = self.owned(&job_id, &assignment_id) {
                    self.active.remove(&job_id);
                    self.job_events
                        .push(JobEvent::Completed {
                            job_id,
                            assignment_id,
                            report,
                        })
                        .await;
                }
                true
            }
            ClientMessage::JobFailed {
                job_id,
                assignment_id,
                error,
                kind,
                missing_paths,
                spans,
                elapsed_ms,
                metrics,
            } => {
                let report = ReportedTimeline::received(spans, elapsed_ms);
                self.forget_uploads(&job_id, uploads).await;
                self.logs.flush().await;
                if self.owned(&job_id, &assignment_id).is_some()
                    && self.scheduler.cluster_member_released(&job_id).await
                {
                    info!(peer_id = %self.peer_id, %job_id, %error, "held cluster member released before its start");
                    self.active.remove(&job_id);
                } else if let Some(assignment_id) = self.owned(&job_id, &assignment_id) {
                    warn!(peer_id = %self.peer_id, %job_id, %error, ?kind, phases = report.spans.len(), "job failed");
                    self.active.remove(&job_id);
                    self.scheduler.record_job_timeline(
                        assignment_id,
                        DispatchedJobOutcome::Failed,
                        report,
                    );
                    self.job_events
                        .push(JobEvent::Failed {
                            job_id,
                            error,
                            kind,
                            missing_paths,
                            metrics,
                        })
                        .await;
                }
                true
            }
            ClientMessage::Draining => {
                self.on_draining().await;
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
            ClientMessage::EvalCachePull {
                job_id,
                fingerprint,
            } => {
                self.on_eval_cache_pull(job_id, fingerprint).await;
                true
            }
            msg @ (ClientMessage::CacheQuery { .. }
            | ClientMessage::QueryKnownDerivations { .. }) => {
                self.rpc().serve(msg);
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
            ClientMessage::UploadRequest {
                job_id,
                request_id,
                object,
                size,
            } => {
                self.on_upload_request(job_id, request_id, object, size, uploads)
                    .await;
                true
            }
            ClientMessage::UploadCancel { request_id } => {
                self.on_upload_cancel(request_id, uploads).await;
                true
            }
            ClientMessage::UploadFinished {
                request_id,
                metadata,
            } => {
                self.on_upload_finished(request_id, metadata, uploads).await;
                true
            }
            ClientMessage::UploadChunk {
                request_id,
                data,
                offset,
                is_final,
            } => {
                self.on_upload_chunk(request_id, offset, &data, is_final, uploads)
                    .await;
                true
            }
            ClientMessage::LogChunk {
                job_id,
                task_index,
                data,
            } => {
                self.on_log_chunk(&job_id, task_index, &data).await;
                true
            }
        }
    }

    fn owned(&self, job_id: &str, reported: &str) -> Option<DispatchedJobId> {
        let current = self.active.assignment_id(job_id);
        if assignment_matches(current, reported) {
            return current;
        }

        warn!(peer_id = %self.peer_id, %job_id, reported, ?current, "report from another dispatch; dropped");
        None
    }

    fn owns(&self, job_id: &str, reported: &str) -> bool {
        self.owned(job_id, reported).is_some()
    }

    fn rpc(&self) -> RpcContext {
        RpcContext {
            state: Arc::clone(self.state),
            scheduler: Arc::clone(self.scheduler),
            writer: self.writer.clone(),
            peer_id: self.peer_id.to_owned(),
            active: self.active.clone(),
        }
    }

    fn spawn_worker_metrics(&self, metrics: WorkerMetrics) {
        let rpc = self.rpc();
        self.state.shutdown.spawn(async move {
            rpc.on_worker_metrics(metrics).await;
        });
    }

    async fn on_eval_cache_pull(&mut self, job_id: String, fingerprint: String) {
        handle_eval_cache_pull(self.state, self.writer, job_id, fingerprint).await;
    }

    async fn on_eval_message(
        &mut self,
        job_id: String,
        level: gradient_wire::types::EvalMessageLevel,
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

    async fn on_reauth_request(&mut self) -> bool {
        debug!(peer_id = %self.peer_id, "ReauthRequest");
        if let Some(dialed) = self.dialed {
            return refresh_dialed_peers(
                self.writer,
                self.state,
                self.scheduler,
                self.peer_id,
                dialed,
            )
            .await;
        }

        let (_, registered_peers) = challenge_for(self.state, self.peer_id).await;
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
        let (team, registered_peers) = challenge_for(self.state, self.peer_id).await;
        let resolved = resolve_authorized(self.state, &team, &registered_peers, &tokens).await;

        if resolved.authorized.is_empty() {
            info!(peer_id = %self.peer_id, "no project authorizes this worker any more - disconnecting");
            let _ = send_server_msg(
                self.writer,
                &ServerMessage::Reject {
                    code: 403,
                    reason: "no project authorizes this worker".into(),
                },
            )
            .await;
            return false;
        }

        let updated: HashSet<ProjectId> = resolved
            .authorized
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        self.scheduler
            .update_authorized_peers(self.peer_id, updated)
            .await;
        send_server_msg(
            self.writer,
            &ServerMessage::AuthUpdate {
                authorized_peers: resolved.authorized,
                failed_peers: resolved.failed,
            },
        )
        .await
        .is_ok()
    }

    async fn on_worker_capabilities(&mut self, caps: WorkerCapabilities) {
        debug!(peer_id = %self.peer_id, ?caps, "WorkerCapabilities");
        self.scheduler
            .update_worker_capabilities(self.peer_id, caps)
            .await;
    }

    async fn on_request_job_list(&mut self) -> bool {
        debug!(peer_id = %self.peer_id, "RequestJobList");
        let candidates = self.scheduler.get_job_candidates(self.peer_id).await;
        self.send_job_list_chunks(candidates).await
    }

    async fn send_job_list_chunks(
        &mut self,
        candidates: Vec<gradient_wire::messages::JobCandidate>,
    ) -> bool {
        use gradient_wire::messages::ServerMessage;
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

    #[tracing::instrument(level = "debug", skip_all, fields(?kind, job_id = tracing::field::Empty))]
    async fn on_request_job(&mut self, kind: JobKind) -> bool {
        debug!(peer_id = %self.peer_id, ?kind, "RequestJob");
        match self.scheduler.request_job(self.peer_id, kind).await {
            Some(assignment) => self.hand_out(assignment, None).await,
            None => true,
        }
    }

    pub(super) async fn hand_out(
        &mut self,
        assignment: Assignment,
        cluster: Option<ClusterMembership>,
    ) -> bool {
        tracing::Span::current().record("job_id", assignment.job_id());
        self.active.insert(
            assignment.job_id().to_owned(),
            ActiveJob {
                assignment_id: assignment.assignment_id(),
                pending: assignment.pending.clone(),
                cluster: cluster.as_ref().and_then(|m| m.attempt.parse().ok()),
            },
        );
        send_credentials_for_job(
            self.writer,
            self.state,
            self.scheduler,
            self.peer_id,
            &assignment.job,
            assignment.project_id,
        )
        .instrument(debug_span!("send_credentials"))
        .await;
        let job_id = assignment.job_id().to_owned();
        let assigned = debug_span!("assign_job", %job_id);
        send_server_msg(
            self.writer,
            &ServerMessage::AssignJob {
                job_id,
                assignment_id: assignment.assignment_id().to_string(),
                job: assignment.job,
                cluster: cluster.map(Box::new),
            },
        )
        .instrument(assigned)
        .await
        .is_ok()
    }

    #[tracing::instrument(level = "debug", skip_all, fields(scores = scores.len(), is_final))]
    async fn on_request_job_chunk(&mut self, scores: Vec<CandidateScore>, is_final: bool) {
        debug!(peer_id = %self.peer_id, count = scores.len(), is_final, "RequestJobChunk");
        self.scheduler.record_scores(self.peer_id, scores).await;
    }

    async fn on_assign_job_response(
        &mut self,
        job_id: String,
        accepted: bool,
        reason: Option<String>,
    ) {
        if accepted {
            info!(peer_id = %self.peer_id, %job_id, "job accepted");
            self.scheduler.cluster_member_accepted(&job_id).await;
        } else {
            info!(peer_id = %self.peer_id, %job_id, ?reason, "job rejected by worker");
            self.withdraw_assignment(&job_id).await;
            if !self.scheduler.cluster_member_rejected(&job_id).await {
                self.scheduler.job_rejected(self.peer_id, &job_id).await;
            }
        }
    }

    async fn withdraw_assignment(&mut self, job_id: &str) {
        let Some(active) = self.active.remove(job_id) else {
            return;
        };

        if let Err(e) = gradient_db::scheduling::assignment_record::abandon_open_assignment(
            &self.state.worker_db,
            active.assignment_id,
        )
        .await
        {
            warn!(peer_id = %self.peer_id, %job_id, dispatch = %active.assignment_id, error = %e, "rejected assignment left its dispatch row open");
        }
    }

    async fn on_draining(&mut self) {
        info!(peer_id = %self.peer_id, "worker draining");
        self.scheduler.mark_worker_draining(self.peer_id).await;
    }

    async fn on_log_chunk(&mut self, job_id: &str, task_index: u32, data: &[u8]) {
        debug!(peer_id = %self.peer_id, %job_id, task_index, bytes = data.len(), "LogChunk");
        match self.active.build(job_id, task_index) {
            Some(build) => self.logs.append(build, data.to_vec()).await,
            None => {
                debug!(peer_id = %self.peer_id, %job_id, task_index, "log chunk dropped: not a build this session runs")
            }
        }
    }

    fn on_build_progress(&self, build_id: &str, progress: BuildProgress) {
        let Ok(shared_build) = build_id.parse::<DerivationBuildId>() else {
            warn!(peer_id = %self.peer_id, %build_id, "invalid derivation_build in BuildProgress");
            return;
        };
        self.state
            .build_progress
            .set(shared_build, progress, Instant::now());
        self.state.events.publish(build::Progress {
            derivation_build: shared_build,
            progress,
        });
    }

    fn on_eval_progress(&self, job_id: &str, progress: WireEvalProgress) {
        let Some((evaluation_id, task)) = self.active.eval(job_id) else {
            return;
        };
        let progress = evaluation_progress(progress);
        self.state
            .eval_progress
            .set(evaluation_id, progress.clone(), Instant::now());
        self.state.events.publish(evaluation::Activity {
            evaluation_id,
            task,
            progress,
        });
    }

    async fn on_nar_request(&mut self, job_id: String, paths: Vec<String>) {
        debug!(peer_id = %self.peer_id, %job_id, count = paths.len(), "NarRequest");
        let shutdown = self.state.shutdown.clone();
        for store_path in paths {
            let state = Arc::clone(self.state);
            let writer = self.writer.clone();
            let permit = Arc::clone(self.nar_serve_semaphore);
            let peer_id = self.peer_id.to_owned();
            let job_id = job_id.clone();
            shutdown.spawn(async move {
                let server = Arc::clone(&state.nar_downloads);
                let Some(_slot) = ServeSlot::acquire(permit, server).await else {
                    return;
                };

                if let Err(e) =
                    serve_nar_request(&state, &writer, &job_id, &store_path, 0, None).await
                {
                    warn!(%peer_id, %job_id, %store_path, error = %e, "NarRequest serve failed");
                }
            });
        }
    }

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
            let server = Arc::clone(&state.nar_downloads);
            let Some(_slot) = ServeSlot::acquire(permit, server).await else {
                return;
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

#[derive(Clone)]
pub(super) struct RpcContext {
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    writer: ProtoWriter,
    peer_id: String,
    active: ActiveJobs,
}

impl RpcContext {
    pub(super) fn new(
        state: Arc<ServerState>,
        scheduler: Arc<Scheduler>,
        writer: ProtoWriter,
        peer_id: String,
        active: ActiveJobs,
    ) -> Self {
        Self {
            state,
            scheduler,
            writer,
            peer_id,
            active,
        }
    }

    pub(super) fn serve(&self, msg: ClientMessage) -> Option<ClientMessage> {
        let rpc = self.clone();
        match msg {
            ClientMessage::CacheQuery {
                job_id,
                query_id,
                paths,
                nar_sizes,
                mode,
                external,
            } => {
                self.state.shutdown.spawn(async move {
                    rpc.on_cache_query(job_id, query_id, paths, nar_sizes, mode, external)
                        .await
                });
                None
            }
            ClientMessage::QueryKnownDerivations {
                job_id,
                query_id,
                drv_paths,
            } => {
                self.state.shutdown.spawn(async move {
                    rpc.on_query_known_derivations(job_id, query_id, drv_paths)
                        .await
                });
                None
            }
            other => Some(other),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the wire-protocol message fields; refactor tracked in #503"
    )]
    async fn on_cache_query(
        &self,
        job_id: String,
        query_id: String,
        paths: Vec<String>,
        nar_sizes: Vec<Option<u64>>,
        mode: gradient_wire::types::QueryMode,
        external: bool,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %query_id, count = paths.len(), ?mode, external, "CacheQuery");
        let answer = async {
            let project_id = match self.active.project(&job_id) {
                Some(project_id) => Some(project_id),
                None => self.scheduler.project_for_job(&job_id).await,
            };
            handle_cache_query(&self.state, project_id, &paths, &nar_sizes, mode, external).await
        };

        // A DB error or an over-budget handler is indeterminate, never "absent". `CacheError` is
        // making the worker retry instead of failing the eval with `InputsUnavailable`.
        let reply = match tokio::time::timeout(CACHE_QUERY_BUDGET, answer).await {
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

    async fn on_query_known_derivations(
        &self,
        job_id: String,
        query_id: String,
        drv_paths: Vec<String>,
    ) {
        debug!(peer_id = %self.peer_id, %job_id, %query_id, count = drv_paths.len(), "QueryKnownDerivations");
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
        let known = match self.scheduler.active_job(&job_id).await {
            Some(job) if job.prunes_walk() => {
                match self.state.graph.known_derivations(hashes).await {
                    Ok(known) => known,
                    Err(e) => {
                        warn!(peer_id = %self.peer_id, %job_id, error = %e, "QueryKnownDerivations degraded; pruning nothing");
                        vec![]
                    }
                }
            }
            Some(_) => vec![],
            None => {
                warn!(peer_id = %self.peer_id, %job_id, "QueryKnownDerivations: no active job");
                vec![]
            }
        };
        debug!(peer_id = %self.peer_id, %job_id, %query_id, known = known.len(), "KnownDerivations");
        if send_server_msg(
            &self.writer,
            &ServerMessage::KnownDerivations { query_id, known },
        )
        .await
        .is_err()
        {
            debug!(peer_id = %self.peer_id, "KnownDerivations send failed; connection closing");
        }
    }

    async fn on_worker_metrics(&self, metrics: WorkerMetrics) {
        debug!(peer_id = %self.peer_id, ?metrics, "WorkerMetrics");
        self.scheduler
            .update_worker_metrics(&self.peer_id, metrics)
            .await;
    }
}

fn build_progress_phase(phase: WireBuildProgressPhase) -> BuildProgressPhase {
    match phase {
        WireBuildProgressPhase::Download => BuildProgressPhase::Download,
        WireBuildProgressPhase::Prefetch => BuildProgressPhase::Prefetch,
        WireBuildProgressPhase::Upload => BuildProgressPhase::Upload,
    }
}

fn evaluation_progress(progress: WireEvalProgress) -> EvaluationProgress {
    use gradient_types::events::evaluation::{InputFetch, InputFetchState};
    use gradient_wire::types::InputFetchState as Wire;
    match progress {
        WireEvalProgress::Evaluating { thunks } => EvaluationProgress::Evaluating { thunks },
        WireEvalProgress::Fetching { inputs } => EvaluationProgress::Fetching {
            inputs: inputs
                .into_iter()
                .map(|i| InputFetch {
                    name: i.name,
                    state: match i.state {
                        Wire::Queued => InputFetchState::Queued,
                        Wire::Fetching => InputFetchState::Fetching,
                        Wire::Done => InputFetchState::Done,
                        Wire::Failed => InputFetchState::Failed,
                    },
                    downloaded_bytes: i.downloaded_bytes,
                    expected_bytes: i.expected_bytes,
                })
                .collect(),
        },
    }
}

#[cfg(test)]
pub(in crate::handler) mod fixture {
    use super::*;
    use crate::handler::job_events::SchedulerJobEvents;
    use crate::handler::upload::{UploadSession, UploadTable};
    use bytes::Bytes;
    use gradient_scheduler::jobs::PendingEvalJob;
    use gradient_storage::admission::Admitted;
    use gradient_types::ids::{CommitId, EvaluationId};
    use gradient_wire::types::{FlakeJob, FlakeSource, FlakeStep};
    use std::time::Duration;
    use tokio::sync::mpsc;

    pub(in crate::handler) const JOB: &str = "j1";

    #[test]
    fn an_aborted_attempt_leaves_only_its_members_session() {
        let attempt = gradient_types::ids::ClusterAttemptId::now_v7();
        let job = |cluster| ActiveJob {
            assignment_id: DispatchedJobId::now_v7(),
            pending: pending_eval(),
            cluster,
        };
        let active = ActiveJobs::from(HashMap::from([
            ("member".to_owned(), job(Some(attempt))),
            ("single".to_owned(), job(None)),
        ]));

        assert_eq!(active.remove_attempt(attempt), vec!["member".to_owned()]);

        assert!(!active.contains("member"));
        assert!(active.contains("single"));
    }

    pub(in crate::handler) fn pending_eval() -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: EvaluationId::now_v7(),
            task_id: None,
            project_id: ProjectId::now_v7(),
            commit_id: CommitId::now_v7(),
            repository: "https://example.com/repo".into(),
            job: FlakeJob {
                steps: vec![FlakeStep::EvaluateDerivations],
                source: FlakeSource::Repository {
                    url: "https://example.com/repo".into(),
                    commit: "abc123".into(),
                },
                wildcards: vec!["*".into()],
                timeout_secs: None,
                input_overrides: vec![],
                input_update: None,
            },
            required_paths: vec![],
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            rescore_count: 0,
            history: Default::default(),
            walk_mode: Default::default(),
            prioritized: false,
            build_request: false,
        })
    }

    pub(in crate::handler) struct TestSession {
        pub writer: ProtoWriter,
        pub state: Arc<ServerState>,
        pub scheduler: Arc<Scheduler>,
        pub semaphore: Arc<Semaphore>,
        pub job_events: JobEvents,
        pub logs: LogLane,
        pub active: ActiveJobs,
        pub uploads: UploadSession,
    }

    impl TestSession {
        pub(in crate::handler) async fn new(
            state: &Arc<ServerState>,
        ) -> (
            Self,
            mpsc::Receiver<Bytes>,
            mpsc::UnboundedReceiver<Admitted>,
        ) {
            let scheduler = Arc::new(Scheduler::new(Arc::clone(state)));
            scheduler.spawn_core(None).await.expect("core actor");
            let (writer, sent) = ProtoWriter::spy(Duration::from_secs(5));
            let job_events = JobEvents::spawn(
                &state.shutdown,
                "w1",
                SchedulerJobEvents {
                    scheduler: Arc::clone(&scheduler),
                    writer: writer.clone(),
                    peer_id: "w1".into(),
                },
            );
            let active = ActiveJobs::from(HashMap::from([(
                JOB.to_owned(),
                ActiveJob {
                    assignment_id: DispatchedJobId::now_v7(),
                    pending: pending_eval(),
                    cluster: None,
                },
            )]));
            let (admission, admitted) = state.upload_admission.open_session("test");
            let uploads = UploadSession {
                admission,
                table: UploadTable::default(),
                partials: gradient_storage::PartialStore::new(
                    tempfile::TempDir::new().unwrap().keep(),
                )
                .unwrap(),
                retain_up_to: 0,
                idle_lease: Duration::from_secs(300),
            };
            let session = Self {
                writer,
                state: Arc::clone(state),
                scheduler,
                semaphore: Arc::new(Semaphore::new(1)),
                job_events,
                logs: LogLane::spawn(&state.shutdown, |_, _| async {}),
                active,
                uploads,
            };
            (session, sent, admitted)
        }

        pub(in crate::handler) fn split(&mut self) -> (InboundContext<'_>, &mut UploadSession) {
            (
                InboundContext {
                    writer: &self.writer,
                    state: &self.state,
                    scheduler: &self.scheduler,
                    peer_id: "w1",
                    nar_serve_semaphore: &self.semaphore,
                    active: &self.active,
                    job_events: &self.job_events,
                    logs: &self.logs,
                    dialed: None,
                },
                &mut self.uploads,
            )
        }
    }

    pub(in crate::handler) fn decode(bytes: Bytes) -> ServerMessage {
        gradient_wire::codec::from_bytes::<ServerMessage>(
            bytes,
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .expect("deserialise ServerMessage")
    }
}

#[cfg(test)]
mod assignment_id_tests {
    use super::assignment_matches;
    use gradient_types::ids::DispatchedJobId;

    #[test]
    fn only_the_current_assignment_is_accepted() {
        let current = DispatchedJobId::now_v7();
        let other = DispatchedJobId::now_v7();
        assert!(assignment_matches(Some(current), &current.to_string()));
        assert!(!assignment_matches(Some(current), &other.to_string()));
        assert!(!assignment_matches(None, &current.to_string()));
        assert!(!assignment_matches(Some(current), "not-a-uuid"));
    }
}

#[cfg(test)]
mod assignment_response_tests {
    use super::fixture::pending_eval;
    use super::*;
    use crate::handler::job_events::SchedulerJobEvents;
    use gradient_test_support::prelude::*;
    use gradient_types::events::Event;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::time::Duration;

    fn detached_writer() -> ProtoWriter {
        ProtoWriter::spy(Duration::from_secs(1)).0
    }

    #[tokio::test]
    async fn a_rejected_assignment_closes_its_assignment_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let log_db = db.clone();
        let state = test_state(db);
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.expect("core actor");
        let writer = detached_writer();
        let semaphore = Arc::new(Semaphore::new(1));
        let job_events = JobEvents::spawn(
            &state.shutdown,
            "w1",
            SchedulerJobEvents {
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: "w1".into(),
            },
        );
        let assignment_id = DispatchedJobId::now_v7();
        let active = ActiveJobs::from(HashMap::from([(
            "j1".to_owned(),
            ActiveJob {
                assignment_id,
                pending: pending_eval(),
                cluster: None,
            },
        )]));

        let mut ctx = InboundContext {
            writer: &writer,
            state: &state,
            scheduler: &scheduler,
            peer_id: "w1",
            nar_serve_semaphore: &semaphore,
            active: &active,
            job_events: &job_events,
            logs: &LogLane::spawn(&state.shutdown, |_, _| async {}),
            dialed: None,
        };
        ctx.on_assign_job_response("j1".into(), false, Some("at capacity".into()))
            .await;

        assert!(active.is_empty());
        let log = log_db.into_transaction_log();
        let close = log
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| s.sql.starts_with("UPDATE \"dispatched_job\""))
            .expect("the rejection closes the row it was handed");
        assert!(
            close.sql.contains("\"finished_at\" IS NULL"),
            "{}",
            close.sql
        );
        assert!(
            format!("{:?}", close.values).contains(&assignment_id.to_string()),
            "{:?}",
            close.values
        );
    }

    #[tokio::test]
    async fn an_accepted_assignment_keeps_its_row_open() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let log_db = db.clone();
        let state = test_state(db);
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.expect("core actor");
        let writer = detached_writer();
        let semaphore = Arc::new(Semaphore::new(1));
        let job_events = JobEvents::spawn(
            &state.shutdown,
            "w1",
            SchedulerJobEvents {
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: "w1".into(),
            },
        );
        let active = ActiveJobs::from(HashMap::from([(
            "j1".to_owned(),
            ActiveJob {
                assignment_id: DispatchedJobId::now_v7(),
                pending: pending_eval(),
                cluster: None,
            },
        )]));

        let mut ctx = InboundContext {
            writer: &writer,
            state: &state,
            scheduler: &scheduler,
            peer_id: "w1",
            nar_serve_semaphore: &semaphore,
            active: &active,
            job_events: &job_events,
            logs: &LogLane::spawn(&state.shutdown, |_, _| async {}),
            dialed: None,
        };
        ctx.on_assign_job_response("j1".into(), true, None).await;

        assert!(active.contains("j1"));
        assert!(log_db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn a_progress_report_is_held_in_memory_and_broadcast() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let log_db = db.clone();
        let state = test_state(db);
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let writer = detached_writer();
        let semaphore = Arc::new(Semaphore::new(1));
        let job_events = JobEvents::spawn(
            &state.shutdown,
            "w1",
            SchedulerJobEvents {
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: "w1".into(),
            },
        );
        let active = ActiveJobs::from(HashMap::new());
        let mut events = state.events.subscribe();
        let shared_build = DerivationBuildId::now_v7();
        let progress = BuildProgress {
            phase: BuildProgressPhase::Upload,
            bytes_done: 512,
            bytes_total: Some(2048),
            paths_done: 1,
            paths_total: Some(2),
        };

        let ctx = InboundContext {
            writer: &writer,
            state: &state,
            scheduler: &scheduler,
            peer_id: "w1",
            nar_serve_semaphore: &semaphore,
            active: &active,
            job_events: &job_events,
            logs: &LogLane::spawn(&state.shutdown, |_, _| async {}),
            dialed: None,
        };
        ctx.on_build_progress(&shared_build.to_string(), progress);
        ctx.on_build_progress("not-a-uuid", progress);

        assert_eq!(
            state.build_progress.get(&shared_build, Instant::now()),
            Some(progress)
        );
        match events.try_recv().map(|env| env.event.clone()) {
            Ok(Event::BuildProgress(sent)) => {
                assert_eq!(sent.derivation_build, shared_build);
                assert_eq!(sent.progress, progress);
            }
            other => panic!("expected BuildProgress, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
        assert!(log_db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn an_eval_progress_report_is_held_in_memory_and_broadcast() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let writer = detached_writer();
        let semaphore = Arc::new(Semaphore::new(1));
        let job_events = JobEvents::spawn(
            &state.shutdown,
            "w1",
            SchedulerJobEvents {
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: "w1".into(),
            },
        );
        let pending = pending_eval();
        let PendingJob::Eval(job) = &pending else {
            unreachable!()
        };
        let (evaluation_id, task) = (job.evaluation_id, job.task_id);
        let active = ActiveJobs::from(HashMap::from([(
            "job-1".to_owned(),
            ActiveJob {
                assignment_id: DispatchedJobId::now_v7(),
                pending,
                cluster: None,
            },
        )]));
        let mut events = state.events.subscribe();

        let ctx = InboundContext {
            writer: &writer,
            state: &state,
            scheduler: &scheduler,
            peer_id: "w1",
            nar_serve_semaphore: &semaphore,
            active: &active,
            job_events: &job_events,
            logs: &LogLane::spawn(&state.shutdown, |_, _| async {}),
            dialed: None,
        };
        ctx.on_eval_progress("job-1", WireEvalProgress::Evaluating { thunks: 42 });
        ctx.on_eval_progress("unknown-job", WireEvalProgress::Evaluating { thunks: 1 });

        let expected = EvaluationProgress::Evaluating { thunks: 42 };
        assert_eq!(
            state.eval_progress.get(&evaluation_id, Instant::now()),
            Some(expected.clone())
        );
        match events.try_recv().map(|env| env.event.clone()) {
            Ok(Event::EvaluationActivity(sent)) => {
                assert_eq!(
                    (sent.evaluation_id, sent.task, sent.progress),
                    (evaluation_id, task, expected)
                );
            }
            other => panic!("expected evaluation.activity, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_cache_query_for_an_owned_job_is_answered_without_the_scheduler() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let (writer, mut sent) = ProtoWriter::spy(Duration::from_secs(1));
        let semaphore = Arc::new(Semaphore::new(1));
        let job_events = JobEvents::spawn(
            &state.shutdown,
            "w1",
            SchedulerJobEvents {
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: "w1".into(),
            },
        );
        let active = ActiveJobs::from(HashMap::from([(
            "j1".to_owned(),
            ActiveJob {
                assignment_id: DispatchedJobId::now_v7(),
                pending: pending_eval(),
                cluster: None,
            },
        )]));

        let ctx = InboundContext {
            writer: &writer,
            state: &state,
            scheduler: &scheduler,
            peer_id: "w1",
            nar_serve_semaphore: &semaphore,
            active: &active,
            job_events: &job_events,
            logs: &LogLane::spawn(&state.shutdown, |_, _| async {}),
            dialed: None,
        };
        assert!(
            ctx.rpc()
                .serve(ClientMessage::CacheQuery {
                    job_id: "j1".into(),
                    query_id: "q1".into(),
                    paths: vec!["/nix/store/00000000000000000000000000000000-a".into()],
                    nar_sizes: vec![None],
                    mode: gradient_wire::types::QueryMode::Pull,
                    external: false,
                })
                .is_none()
        );

        let reply = tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("answered before the scheduler's call timeout")
            .expect("reply sent");
        let reply = gradient_wire::codec::from_bytes::<ServerMessage>(
            reply,
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .expect("deserialise");
        assert!(
            matches!(
                &reply,
                ServerMessage::CacheStatus { query_id, .. } | ServerMessage::CacheError { query_id, .. }
                    if query_id == "q1"
            ),
            "{reply:?}"
        );
    }
}
