/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use gradient_types::events::{build, evaluation};
use gradient_types::ids::{DerivationBuildId, DispatchedJobId, EvaluationId, ProjectId, TaskId};
use gradient_types::{
    BuildProgress, BuildProgressPhase, EvaluationProgress, StorePath, StorePathMapper,
};
use tokio::sync::Semaphore;
use tracing::{Instrument as _, debug, debug_span, info, trace, warn};

use gradient_scheduler::actor::{WorkerCapabilities, WorkerMetrics};
use gradient_scheduler::jobs::{Assignment, PendingJob, Reattached};
use gradient_scheduler::{ReportedTimeline, Scheduler};
use gradient_wire::Rejection;
use gradient_wire::messages::{
    BuildFailureKind, CACHE_QUERY_BUDGET, CandidateScore, ClientMessage, ClusterMembership,
    JobKind, JobUpdateKind, RunningAssignment, ServerMessage,
};
use gradient_wire::types::{
    BuildProgressPhase as WireBuildProgressPhase, EvalProgress as WireEvalProgress,
    EvalShardOutcome,
};

use super::SessionHandles;
use super::auth::{challenge_for, resolve_authorized};
use super::cache::handle_cache_query;
use super::dialed::{DialedSession, refresh_dialed_peers};
use super::eval_cache::handle_eval_cache_pull;
use super::eval_split::{handle_eval_shards_planned, handle_eval_source_ready, send_reply};
use super::job_events::{JobEvent, JobEvents};
use super::log_lane::LogLane;
use super::nar_serve::{ServeSlot, serve_nar_request};
use super::refusal::refuse;
use super::socket::{
    ProtoWriter, job_offer_chunks, send_credentials_for_job, send_error, send_server_msg,
};
use super::store_paths::{
    inbound_or_refuse, outbound_message, outbound_offer, project_mapper, unmapped_project,
};
use super::upload::UploadSession;
use gradient_db::cache_paths::project_read_caches;

#[derive(Clone)]
pub(crate) struct ActiveJob {
    pub assignment_id: DispatchedJobId,
    pub pending: PendingJob,
    pub cluster: Option<gradient_types::ids::ClusterAttemptId>,
}

#[derive(Clone, Default)]
pub(crate) struct ActiveJobs(
    Arc<gradient_util::sync::Mutex<HashMap<String, ActiveJob>>>,
    Arc<gradient_util::sync::Mutex<HashMap<String, ProjectId>>>,
);

impl ActiveJobs {
    pub(crate) fn insert_fetch(&self, fetch_id: String, project: ProjectId) {
        self.1.lock().insert(fetch_id, project);
    }

    pub(crate) fn fetch_project(&self, fetch_id: &str) -> Option<ProjectId> {
        self.1.lock().get(fetch_id).copied()
    }

    pub(crate) fn remove_fetch(&self, fetch_id: &str) -> Option<ProjectId> {
        self.1.lock().remove(fetch_id)
    }

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

    pub(crate) fn session_project(&self, job_id: &str) -> Option<ProjectId> {
        self.project(job_id).or_else(|| self.fetch_project(job_id))
    }

    async fn job_project(&self, scheduler: &Scheduler, job_id: &str) -> Option<ProjectId> {
        match self.session_project(job_id) {
            Some(project) => Some(project),
            None => scheduler.project_for_job(job_id).await,
        }
    }

    pub(crate) fn build(&self, job_id: &str) -> Option<DerivationBuildId> {
        match &self.0.lock().get(job_id)?.pending {
            PendingJob::Build(j) => Some(j.derivation_build),
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

    pub(crate) fn reattach(&self, reattached: &[Reattached]) {
        let mut jobs = self.0.lock();
        for job in reattached {
            jobs.insert(
                job.job_id.clone(),
                ActiveJob {
                    assignment_id: job.assignment_id,
                    pending: job.job.clone(),
                    cluster: job.cluster,
                },
            );
        }
    }

    pub(crate) fn pending(&self) -> Vec<gradient_scheduler::jobs::Reattached> {
        self.0
            .lock()
            .iter()
            .map(|(id, job)| gradient_scheduler::jobs::Reattached {
                job_id: id.clone(),
                assignment_id: job.assignment_id,
                job: job.pending.clone(),
                cluster: job.cluster,
            })
            .collect()
    }
}

fn parsed_assignment_ids(peer_id: &str, assignments: &[RunningAssignment]) -> Vec<DispatchedJobId> {
    assignments
        .iter()
        .filter_map(|a| match a.assignment_id.parse() {
            Ok(id) => Some(id),
            Err(_) => {
                warn!(%peer_id, assignment_id = %a.assignment_id, "RunningJobs lists an unparsable assignment id");
                None
            }
        })
        .collect()
}

impl From<HashMap<String, ActiveJob>> for ActiveJobs {
    fn from(jobs: HashMap<String, ActiveJob>) -> Self {
        Self(
            Arc::new(gradient_util::sync::Mutex::new(jobs)),
            Arc::default(),
        )
    }
}

pub(super) struct InboundContext<'a> {
    pub writer: &'a ProtoWriter,
    pub handles: &'a SessionHandles,
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
        let Some((msg, mapper)) = self.admit(msg).await else {
            return true;
        };
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
                build_cores,
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
                    build_cores,
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
                build_peak_ram_mb,
                lifetime_left_secs,
            } => {
                self.spawn_worker_metrics(WorkerMetrics {
                    cpu_usage_pct,
                    ram_free_mb,
                    disk_speed_mbps,
                    upload_speed_mbps,
                    download_speed_mbps,
                    build_peak_ram_mb,
                    exits_at: lifetime_left_secs
                        .map(|secs| Instant::now() + std::time::Duration::from_secs(secs)),
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
                if self.owns(&job_id, &assignment_id).await {
                    trace!(peer_id = %self.peer_id, %job_id, ?update, "JobUpdate");
                    self.on_job_update(job_id, update).await;
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
                if self.owns(&job_id, &assignment_id).await {
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
                if self.owns(&job_id, &assignment_id).await {
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
                let logs_flushed = self.logs.flushed().await;
                if let Some(assignment_id) = self.owned(&job_id, &assignment_id).await {
                    self.active.remove(&job_id);
                    self.job_events
                        .push(JobEvent::Completed {
                            job_id,
                            assignment_id,
                            report,
                            logs_flushed,
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
                let logs_flushed = self.logs.flushed().await;
                let Some(assignment_id) = self.owned(&job_id, &assignment_id).await else {
                    return true;
                };
                self.active.remove(&job_id);
                if self.scheduler.cluster_member_released(&job_id).await {
                    info!(peer_id = %self.peer_id, %job_id, %error, "waiting cluster member released before its start");
                } else {
                    warn!(peer_id = %self.peer_id, %job_id, %error, ?kind, phases = report.spans.len(), "job failed");
                    self.scheduler
                        .close_failed_job_timeline(assignment_id, kind, report);
                    self.job_events
                        .push(JobEvent::Failed {
                            job_id,
                            error,
                            kind,
                            missing_paths,
                            metrics,
                            logs_flushed,
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
                if let Some(mapper) = mapper {
                    self.on_nar_request(job_id, paths, mapper).await;
                }
                true
            }
            ClientMessage::NarRequestResume {
                job_id,
                store_path,
                received_bytes,
                stream_token,
            } => {
                if let Some(mapper) = mapper {
                    self.on_nar_request_resume(
                        job_id,
                        store_path,
                        received_bytes,
                        stream_token,
                        mapper,
                    )
                    .await;
                }
                true
            }
            ClientMessage::EvalCachePull {
                job_id,
                fingerprint,
                applied,
            } => {
                self.on_eval_cache_pull(job_id, fingerprint, applied).await;
                true
            }
            ClientMessage::EvalSourceReady {
                job_id,
                store_path,
                fingerprint,
            } => {
                if self.owns_split_member(&job_id).await {
                    handle_eval_source_ready(self.scheduler, job_id, store_path, fingerprint).await;
                }
                true
            }
            ClientMessage::EvalShardsPlanned { job_id, shards } => {
                if self.owns_split_member(&job_id).await {
                    handle_eval_shards_planned(self.scheduler, job_id, shards).await;
                }
                true
            }
            ClientMessage::EvalShardRequest {
                job_id,
                have_source,
                finished,
            } => {
                if self.owns_split_member(&job_id).await {
                    self.job_events
                        .push(JobEvent::ShardRequest {
                            job_id,
                            have_source,
                            finished,
                        })
                        .await;
                } else {
                    send_reply(self.writer, job_id, EvalShardOutcome::Finished).await;
                }
                true
            }
            msg @ (ClientMessage::CacheQuery { .. }
            | ClientMessage::QueryKnownDerivations { .. }) => {
                self.rpc().serve(msg);
                true
            }
            ClientMessage::ImportRequest {
                job_id,
                request_id,
                drv_paths,
            } => {
                let owned = self.owns_handed_dispatch(&job_id).await;
                match self.active.eval(&job_id) {
                    Some((evaluation, _)) if owned => {
                        self.job_events
                            .push(JobEvent::ImportRequest {
                                job_id,
                                evaluation,
                                request_id,
                                drv_paths,
                            })
                            .await;
                    }
                    _ => {
                        warn!(peer_id = %self.peer_id, %job_id, "import request for an evaluation this session does not run");
                    }
                }
                true
            }
            ClientMessage::InputRequest {
                job_id,
                request_id,
                request,
            } => {
                if self.owns_handed_dispatch(&job_id).await && self.active.eval(&job_id).is_some() {
                    self.job_events
                        .push(JobEvent::InputRequest {
                            job_id,
                            request_id,
                            request,
                        })
                        .await;
                } else {
                    warn!(peer_id = %self.peer_id, %job_id, "input request for an evaluation this session does not run");
                }
                true
            }
            ClientMessage::InputFetched { fetch_id, outcome } => {
                if self.active.remove_fetch(&fetch_id).is_some() {
                    self.job_events
                        .push(JobEvent::InputFetched { fetch_id, outcome })
                        .await;
                } else {
                    warn!(peer_id = %self.peer_id, %fetch_id, "answer for an input fetch this session does not run");
                }
                true
            }
            ClientMessage::InputFetchProgress {
                fetch_id,
                bytes_done,
                bytes_total,
            } => {
                if self.active.fetch_project(&fetch_id).is_some() {
                    let scheduler = Arc::clone(self.scheduler);
                    let peer_id = self.peer_id.to_owned();
                    self.handles.db.shutdown.spawn(async move {
                        scheduler
                            .handle_input_fetch_progress(
                                &peer_id,
                                &fetch_id,
                                bytes_done,
                                bytes_total,
                            )
                            .await;
                    });
                }
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
            ClientMessage::LogChunk { job_id, data } => {
                self.on_log_chunk(&job_id, &data).await;
                true
            }
            ClientMessage::RunningJobs { assignments } => self.on_running_jobs(assignments).await,
        }
    }

    async fn admit(&self, msg: ClientMessage) -> Option<(ClientMessage, Option<StorePathMapper>)> {
        let project = match &msg {
            ClientMessage::InputFetched { fetch_id, .. } => self.active.fetch_project(fetch_id),
            ClientMessage::JobUpdate { job_id, .. }
            | ClientMessage::JobFailed { job_id, .. }
            | ClientMessage::NarRequest { job_id, .. }
            | ClientMessage::NarRequestResume { job_id, .. }
            | ClientMessage::EvalCachePull { job_id, .. }
            | ClientMessage::ImportRequest { job_id, .. }
            | ClientMessage::InputRequest { job_id, .. }
            | ClientMessage::UploadRequest { job_id, .. } => {
                let project = self.active.job_project(self.scheduler, job_id).await;
                if project.is_none() {
                    refuse(self.writer, &msg, "the job does not resolve to a project").await;
                    return None;
                }
                project
            }
            _ => None,
        };
        let Some(project) = project else {
            return Some((msg, None));
        };
        let mapper = match self.handles.db.project_mapper(project).await {
            Ok(mapper) => mapper,
            Err(e) => {
                refuse(self.writer, &msg, &unmapped_project(&e)).await;
                return None;
            }
        };
        let mapped = inbound_or_refuse(self.writer, &mapper, msg).await?;
        Some((mapped, Some(mapper)))
    }

    async fn on_job_update(&self, job_id: String, update: JobUpdateKind) {
        let starts_build = matches!(update, JobUpdateKind::Building { .. });
        let event = JobEvent::Update { job_id, update };
        if starts_build {
            let applied = self.job_events.push_awaited(event).await;
            self.logs.after(applied).await;
        } else {
            self.job_events.push(event).await;
        }
    }

    async fn owned(&self, job_id: &str, reported: &str) -> Option<DispatchedJobId> {
        if let Ok(assignment_id) = reported.parse::<DispatchedJobId>()
            && self
                .scheduler
                .owns_dispatch(self.peer_id, job_id, assignment_id)
                .await
        {
            return Some(assignment_id);
        }

        warn!(peer_id = %self.peer_id, %job_id, reported, "report from another dispatch; dropped");
        None
    }

    async fn owns(&self, job_id: &str, reported: &str) -> bool {
        self.owned(job_id, reported).await.is_some()
    }

    async fn owns_handed_dispatch(&self, job_id: &str) -> bool {
        match self.active.assignment_id(job_id) {
            Some(assignment_id) => {
                self.scheduler
                    .owns_dispatch(self.peer_id, job_id, assignment_id)
                    .await
            }
            None => false,
        }
    }

    async fn owns_split_member(&self, job_id: &str) -> bool {
        let owned = self.owns_handed_dispatch(job_id).await;
        if !owned {
            warn!(peer_id = %self.peer_id, %job_id, "split evaluation message for a job this session does not run");
        }

        owned
    }

    fn rpc(&self) -> RpcContext {
        RpcContext {
            handles: self.handles.clone(),
            scheduler: Arc::clone(self.scheduler),
            writer: self.writer.clone(),
            peer_id: self.peer_id.to_owned(),
            active: self.active.clone(),
        }
    }

    fn spawn_worker_metrics(&self, metrics: WorkerMetrics) {
        let rpc = self.rpc();
        self.handles.db.shutdown.spawn(async move {
            rpc.on_worker_metrics(metrics).await;
        });
    }

    async fn on_eval_cache_pull(
        &mut self,
        job_id: String,
        fingerprint: String,
        applied: Vec<String>,
    ) {
        let db = &self.handles.db.worker_db;
        let readable = match self.active.job_project(self.scheduler, &job_id).await {
            Some(project) => project_read_caches(db, project).await.unwrap_or_else(|e| {
                warn!(peer_id = %self.peer_id, %job_id, error = %e, "readable caches lookup failed");
                Vec::new()
            }),
            None => Vec::new(),
        };
        handle_eval_cache_pull(
            &self.handles.db.storage.nar_storage,
            self.writer,
            &readable,
            job_id,
            fingerprint,
            applied,
        )
        .await;
    }

    async fn on_eval_message(
        &mut self,
        job_id: String,
        level: gradient_wire::types::EvalMessageLevel,
        source: String,
        message: String,
    ) {
        trace!(peer_id = %self.peer_id, %job_id, ?level, %source, "EvalMessage");
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
                &self.handles.db.worker_db,
                self.scheduler,
                self.peer_id,
                dialed,
            )
            .await;
        }

        let (_, registered_peers) = challenge_for(&self.handles.db.worker_db, self.peer_id).await;
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
        let db = &self.handles.db.worker_db;
        let (team, registered_peers) = challenge_for(db, self.peer_id).await;
        let resolved = resolve_authorized(db, &team, &registered_peers, &tokens).await;

        if resolved.authorized.is_empty() {
            info!(peer_id = %self.peer_id, "no project authorizes this worker any more - disconnecting");
            let _ = send_server_msg(
                self.writer,
                &ServerMessage::from(Rejection::NoProjectAuthorization),
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
        let offer = self.scheduler.get_job_candidates(self.peer_id).await;
        self.send_job_list_chunks(outbound_offer(offer, &self.handles.db).await)
            .await
    }

    async fn send_job_list_chunks(
        &mut self,
        candidates: Vec<gradient_wire::messages::JobCandidate>,
    ) -> bool {
        use gradient_wire::messages::ServerMessage;
        let chunks = job_offer_chunks(&candidates);
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
        trace!(peer_id = %self.peer_id, ?kind, "RequestJob");
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
        let Some(mapper) = project_mapper(&self.handles.db, assignment.project_id).await else {
            return self.fail_unmappable(assignment.job_id()).await;
        };
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
            &self.handles.db,
            self.scheduler,
            self.peer_id,
            &assignment.job,
            assignment.project_id,
        )
        .instrument(debug_span!("send_credentials"))
        .await;
        let job_id = assignment.job_id().to_owned();
        let assigned = debug_span!("assign_job", %job_id);
        let assign = ServerMessage::AssignJob {
            job_id,
            assignment_id: assignment.assignment_id().to_string(),
            job: assignment.job,
            cluster: cluster.map(Box::new),
        };
        send_server_msg(self.writer, &outbound_message(assign, &mapper))
            .instrument(assigned)
            .await
            .is_ok()
    }

    async fn fail_unmappable(&self, job_id: &str) -> bool {
        let failed = self
            .scheduler
            .handle_job_failed(
                self.peer_id,
                job_id,
                "the project's store paths cannot be mapped",
                BuildFailureKind::Transient,
                &[],
                None,
            )
            .await;
        if let Err(e) = failed {
            warn!(peer_id = %self.peer_id, %job_id, error = %e, "releasing an unmappable job failed");
        }
        true
    }

    #[tracing::instrument(level = "debug", skip_all, fields(scores = scores.len(), is_final))]
    async fn on_request_job_chunk(&mut self, scores: Vec<CandidateScore>, is_final: bool) {
        trace!(peer_id = %self.peer_id, count = scores.len(), is_final, "RequestJobChunk");
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

    async fn on_running_jobs(&mut self, assignments: Vec<RunningAssignment>) -> bool {
        if self.dialed.is_some() {
            debug!(peer_id = %self.peer_id, listed = assignments.len(), "RunningJobs of a dialed session ignored");
            return true;
        }
        let listed = parsed_assignment_ids(self.peer_id, &assignments);
        let reclaimed = self
            .scheduler
            .reclaim_assignments(self.peer_id, listed)
            .await;
        info!(peer_id = %self.peer_id, reattached = reclaimed.reattached.len(), unknown = reclaimed.unknown.len(), "RunningJobs");
        self.active.reattach(&reclaimed.reattached);
        self.abort_unknown(reclaimed.unknown).await
    }

    async fn abort_unknown(&self, unknown: Vec<(DispatchedJobId, Option<String>)>) -> bool {
        for (assignment_id, key) in unknown {
            let Some(job_id) = key else {
                warn!(peer_id = %self.peer_id, %assignment_id, "RunningJobs lists an assignment without a row");
                continue;
            };
            let abort = ServerMessage::AbortJob {
                job_id,
                reason: "the server no longer runs this assignment".into(),
            };
            if send_server_msg(self.writer, &abort).await.is_err() {
                return false;
            }
        }
        true
    }

    async fn withdraw_assignment(&mut self, job_id: &str) {
        let Some(active) = self.active.remove(job_id) else {
            return;
        };

        if let Err(e) = gradient_db::scheduling::assignment_record::abandon_open_assignment(
            &self.handles.db.worker_db,
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

    async fn on_log_chunk(&mut self, job_id: &str, data: &[u8]) {
        trace!(peer_id = %self.peer_id, %job_id, bytes = data.len(), "LogChunk");
        match self.active.build(job_id) {
            Some(build) => self.logs.append(build, data.to_vec()).await,
            None => {
                debug!(peer_id = %self.peer_id, %job_id, "log chunk dropped: not a build this session runs")
            }
        }
    }

    fn on_build_progress(&self, build_id: &str, progress: BuildProgress) {
        let Ok(shared_build) = build_id.parse::<DerivationBuildId>() else {
            warn!(peer_id = %self.peer_id, %build_id, "invalid derivation_build in BuildProgress");
            return;
        };
        self.handles
            .build_progress
            .set(shared_build, progress, Instant::now());
        self.handles.db.events.publish(build::Progress {
            derivation_build: shared_build,
            progress,
        });
    }

    fn on_eval_progress(&self, job_id: &str, progress: WireEvalProgress) {
        let Some((evaluation_id, task)) = self.active.eval(job_id) else {
            return;
        };
        let progress = evaluation_progress(progress);
        self.handles
            .eval_progress
            .set(evaluation_id, progress.clone(), Instant::now());
        self.handles.db.events.publish(evaluation::Activity {
            evaluation_id,
            task,
            progress,
        });
    }

    async fn on_nar_request(
        &mut self,
        job_id: String,
        paths: Vec<String>,
        mapper: StorePathMapper,
    ) {
        trace!(peer_id = %self.peer_id, %job_id, count = paths.len(), "NarRequest");
        let shutdown = self.handles.db.shutdown.clone();
        for store_path in paths {
            let handles = self.handles.clone();
            let writer = self.writer.clone();
            let permit = Arc::clone(self.nar_serve_semaphore);
            let peer_id = self.peer_id.to_owned();
            let job_id = job_id.clone();
            let mapper = mapper.clone();
            shutdown.spawn(async move {
                let server = Arc::clone(&handles.nar_downloads);
                let Some(_slot) = ServeSlot::acquire(permit, server).await else {
                    return;
                };

                if let Err(e) =
                    serve_nar_request(&handles, &writer, &mapper, &job_id, &store_path, 0, None)
                        .await
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
        mapper: StorePathMapper,
    ) {
        trace!(peer_id = %self.peer_id, %job_id, %store_path, received_bytes, "NarRequestResume");
        let handles = self.handles.clone();
        let writer = self.writer.clone();
        let permit = Arc::clone(self.nar_serve_semaphore);
        let peer_id = self.peer_id.to_owned();
        let shutdown = handles.db.shutdown.clone();
        shutdown.spawn(async move {
            let server = Arc::clone(&handles.nar_downloads);
            let Some(_slot) = ServeSlot::acquire(permit, server).await else {
                return;
            };

            if let Err(e) = serve_nar_request(
                &handles,
                &writer,
                &mapper,
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
    handles: SessionHandles,
    scheduler: Arc<Scheduler>,
    writer: ProtoWriter,
    peer_id: String,
    active: ActiveJobs,
}

impl RpcContext {
    pub(super) fn new(
        handles: SessionHandles,
        scheduler: Arc<Scheduler>,
        writer: ProtoWriter,
        peer_id: String,
        active: ActiveJobs,
    ) -> Self {
        Self {
            handles,
            scheduler,
            writer,
            peer_id,
            active,
        }
    }

    pub(super) fn serve(&self, msg: ClientMessage) -> Option<ClientMessage> {
        if !matches!(
            msg,
            ClientMessage::CacheQuery { .. } | ClientMessage::QueryKnownDerivations { .. }
        ) {
            return Some(msg);
        }
        let rpc = self.clone();
        self.handles
            .db
            .shutdown
            .spawn(async move { rpc.answer(msg).await });
        None
    }

    pub(super) async fn job_project(&self, job_id: &str) -> Option<ProjectId> {
        self.active.job_project(&self.scheduler, job_id).await
    }

    async fn answer(&self, msg: ClientMessage) {
        let Some(project_id) = self.job_project(msg.job_id().unwrap_or_default()).await else {
            refuse(&self.writer, &msg, "the job does not resolve to a project").await;
            return;
        };
        let mapper = match self.handles.db.project_mapper(project_id).await {
            Ok(mapper) => mapper,
            Err(e) => {
                refuse(&self.writer, &msg, &unmapped_project(&e)).await;
                return;
            }
        };
        let Some(msg) = inbound_or_refuse(&self.writer, &mapper, msg).await else {
            return;
        };
        let reply = match msg {
            ClientMessage::CacheQuery {
                job_id,
                query_id,
                paths,
                nar_sizes,
                mode,
                external,
            } => {
                self.on_cache_query(
                    project_id, job_id, query_id, paths, nar_sizes, mode, external,
                )
                .await
            }
            ClientMessage::QueryKnownDerivations {
                job_id,
                query_id,
                drv_paths,
            } => {
                self.on_query_known_derivations(job_id, query_id, drv_paths)
                    .await
            }
            _ => return,
        };
        let variant = reply.variant_name();
        if send_server_msg(&self.writer, &outbound_message(reply, &mapper))
            .await
            .is_err()
        {
            debug!(peer_id = %self.peer_id, variant, "reply send failed; connection closing");
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the wire-protocol message fields; refactor tracked in #503"
    )]
    async fn on_cache_query(
        &self,
        project_id: ProjectId,
        job_id: String,
        query_id: String,
        paths: Vec<String>,
        nar_sizes: Vec<Option<u64>>,
        mode: gradient_wire::types::QueryMode,
        external: bool,
    ) -> ServerMessage {
        trace!(peer_id = %self.peer_id, %job_id, %query_id, count = paths.len(), ?mode, external, "CacheQuery");
        let answer = handle_cache_query(
            &self.handles,
            project_id,
            &paths,
            &nar_sizes,
            mode,
            external,
        );

        // A DB error or an over-budget handler is indeterminate, never "absent". `CacheError` is
        // making the worker retry instead of failing the eval with `InputsUnavailable`.
        match tokio::time::timeout(CACHE_QUERY_BUDGET, answer).await {
            Ok(Ok(cached)) => {
                trace!(peer_id = %self.peer_id, %job_id, %query_id, entries = cached.len(), "CacheStatus");
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
        }
    }

    async fn on_query_known_derivations(
        &self,
        job_id: String,
        query_id: String,
        drv_paths: Vec<String>,
    ) -> ServerMessage {
        trace!(peer_id = %self.peer_id, %job_id, %query_id, count = drv_paths.len(), "QueryKnownDerivations");
        let hashes: Vec<String> = drv_paths
            .iter()
            .filter_map(|p| StorePath::parse_stored(p).ok())
            .filter(StorePath::is_derivation)
            .map(|p| p.hash().to_owned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let known = match self.scheduler.active_job(&job_id).await {
            Some(job) if job.prunes_walk() => {
                match self.handles.graph.known_derivations(hashes).await {
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
        trace!(peer_id = %self.peer_id, %job_id, %query_id, known = known.len(), "KnownDerivations");
        ServerMessage::KnownDerivations { query_id, known }
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
    use gradient_core::ServerState;
    use gradient_scheduler::jobs::{PendingBuildJob, PendingEvalJob};
    use gradient_storage::admission::Admitted;
    use gradient_types::ids::{CommitId, DerivationId, EvaluationId};
    use gradient_wire::types::{BuildJob, BuildSpec, FlakeJob, FlakeSource, FlakeStep};
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

    #[test]
    fn active_jobs_name_the_build_of_build_jobs_only() {
        let shared_build = DerivationBuildId::now_v7();
        let job = |pending| ActiveJob {
            assignment_id: DispatchedJobId::now_v7(),
            pending,
            cluster: None,
        };
        let active = ActiveJobs::from(HashMap::from([
            ("build".to_owned(), job(pending_build(shared_build))),
            ("eval".to_owned(), job(pending_eval())),
        ]));

        assert_eq!(active.build("build"), Some(shared_build));
        assert_eq!(active.build("eval"), None);
        assert_eq!(active.build("gone"), None);
    }

    pub(in crate::handler) fn pending_build(derivation_build: DerivationBuildId) -> PendingJob {
        PendingJob::Build(PendingBuildJob {
            derivation_build,
            derivation: DerivationId::now_v7(),
            evaluation_id: EvaluationId::now_v7(),
            project_id: ProjectId::now_v7(),
            job: BuildJob {
                build: BuildSpec::default(),
                requirement: Default::default(),
            },
            required_paths: vec![],
            dependency_count: 0,
            closure_size: None,
            prefer_local_build: false,
            is_fixed_output: false,
            estimate: Default::default(),
            history_key: None,
            queued_at: gradient_types::now(),
            ready_at: gradient_types::now(),
            rescore_count: 0,
            prioritized: false,
            build_request: false,
            ifd: false,
            pname: None,
            substitute: false,
        })
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
            estimate: Default::default(),
            walk_mode: Default::default(),
            eval_index: 0,
            prioritized: false,
            build_request: false,
        })
    }

    pub(in crate::handler) struct TestSession {
        pub writer: ProtoWriter,
        pub handles: SessionHandles,
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
            let (admission, admitted) = state.memory.upload_admission.open_session("test");
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
                handles: SessionHandles::from_state(state),
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
                    handles: &self.handles,
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
mod assignment_response_tests {
    use super::fixture::pending_eval;
    use super::*;
    use crate::handler::job_events::SchedulerJobEvents;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_test_support::prelude::*;
    use gradient_types::events::Event;
    use sea_orm::MockExecResult;
    use std::time::Duration;

    fn detached_writer() -> ProtoWriter {
        ProtoWriter::spy(Duration::from_secs(1)).0
    }

    #[tokio::test]
    async fn a_rejected_assignment_closes_its_assignment_row() {
        let db = mock_db()
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
            handles: &SessionHandles::from_state(&state),
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

        assert!(active.pending().is_empty());
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
        let db = empty_db();
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
            handles: &SessionHandles::from_state(&state),
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
        let db = empty_db();
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
            handles: &SessionHandles::from_state(&state),
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
            state
                .memory
                .build_progress
                .get(&shared_build, Instant::now()),
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
        let db = empty_db();
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
            handles: &SessionHandles::from_state(&state),
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
            state
                .memory
                .eval_progress
                .get(&evaluation_id, Instant::now()),
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
        let state = test_state(empty_db());
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
            handles: &SessionHandles::from_state(&state),
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

    #[tokio::test]
    async fn a_cache_query_for_a_job_no_session_runs_is_refused_and_the_next_is_answered() {
        use super::fixture::{JOB, TestSession, decode};
        use gradient_entity::EXAMPLE_ORIGINAL_HASH;

        let cache_db = gradient_fixtures::mock::mock_db()
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        let state = TestState::new().cache_db(cache_db).build();
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (ctx, _uploads) = session.split();
        let query = |job_id: &str, query_id: &str| ClientMessage::CacheQuery {
            job_id: job_id.into(),
            query_id: query_id.into(),
            paths: vec![format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-a")],
            nar_sizes: vec![None],
            mode: gradient_wire::types::QueryMode::Normal,
            external: false,
        };
        let mut reply = async || {
            let bytes = tokio::time::timeout(Duration::from_secs(5), sent.recv())
                .await
                .expect("answered")
                .expect("reply sent");
            decode(bytes)
        };

        ctx.rpc().serve(query("unknown", "q1"));
        let refused = reply().await;
        assert!(
            matches!(&refused, ServerMessage::CacheError { query_id, .. } if query_id == "q1"),
            "{refused:?}"
        );

        ctx.rpc().serve(query(JOB, "q2"));
        let answered = reply().await;
        assert!(
            matches!(&answered, ServerMessage::CacheStatus { query_id, .. } if query_id == "q2"),
            "{answered:?}"
        );
    }

    #[tokio::test]
    async fn a_malformed_path_refuses_only_its_message() {
        use super::fixture::{JOB, TestSession, decode};

        let state = test_state(empty_db());
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        let malformed = ClientMessage::JobUpdate {
            job_id: JOB.into(),
            assignment_id: DispatchedJobId::now_v7().to_string(),
            update: JobUpdateKind::EvalResult {
                derivations: vec![gradient_wire::types::DiscoveredDerivation {
                    attr: "a".into(),
                    drv_path: "abc-a.drv".into(),
                    ..Default::default()
                }],
                warnings: vec![],
                errors: vec![],
            },
        };

        assert!(ctx.handle(malformed, uploads).await);
        assert!(matches!(
            decode(sent.try_recv().expect("a refusal")),
            ServerMessage::Error { code: 400, .. }
        ));
    }

    #[tokio::test]
    async fn a_message_whose_project_does_not_resolve_is_refused_in_the_reply_it_waits_for() {
        use super::fixture::{JOB, TestSession, decode};
        use gradient_entity::{EXAMPLE_ORIGINAL_HASH, UnresolvableProjectMapping};
        use gradient_wire::types::ImportOutcome;

        let state = TestState::new()
            .worker_db(empty_db())
            .store_path_mapping(Arc::new(UnresolvableProjectMapping))
            .build();
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        let asked = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello.drv");

        let import = ClientMessage::ImportRequest {
            job_id: JOB.into(),
            request_id: "r1".into(),
            drv_paths: vec![asked.clone()],
        };
        assert!(ctx.handle(import, uploads).await);
        assert!(matches!(
            decode(sent.try_recv().expect("a refusal")),
            ServerMessage::ImportResult {
                request_id,
                outcome: ImportOutcome::Unknown { drv_path },
                ..
            } if request_id == "r1" && drv_path == asked
        ));
    }

    #[tokio::test]
    async fn a_cache_status_names_the_paths_the_worker_asked_for() {
        use super::fixture::{JOB, TestSession, decode};
        use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};

        let stored = gradient_entity::cached_path::Model {
            hash: EXAMPLE_STORED_HASH.into(),
            file_hash: Some("sha256:f".into()),
            confirmed: true,
            ..Default::default()
        };
        let cache_db = gradient_fixtures::mock::mock_db()
            .append_query_results([vec![stored]])
            .into_connection();
        let state = TestState::new()
            .cache_db(cache_db)
            .store_path_mapping(Arc::new(RotatedHashMapping))
            .build();
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (ctx, _uploads) = session.split();
        let asked = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");

        ctx.rpc().serve(ClientMessage::CacheQuery {
            job_id: JOB.into(),
            query_id: "q1".into(),
            paths: vec![asked.clone()],
            nar_sizes: vec![None],
            mode: gradient_wire::types::QueryMode::Normal,
            external: false,
        });
        let reply = tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("answered")
            .expect("reply sent");
        let ServerMessage::CacheStatus { cached, .. } = decode(reply) else {
            panic!("a CacheStatus")
        };
        assert_eq!(cached[0].path, asked);
    }

    #[tokio::test]
    async fn a_malformed_cache_query_path_is_answered_as_not_cached() {
        use super::fixture::{JOB, TestSession, decode};
        use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};

        let stored = gradient_entity::cached_path::Model {
            hash: EXAMPLE_STORED_HASH.into(),
            file_hash: Some("sha256:f".into()),
            confirmed: true,
            ..Default::default()
        };
        let cache_db = gradient_fixtures::mock::mock_db()
            .append_query_results([vec![stored]])
            .into_connection();
        let state = TestState::new()
            .cache_db(cache_db)
            .store_path_mapping(Arc::new(RotatedHashMapping))
            .build();
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (ctx, _uploads) = session.split();
        let asked = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");

        ctx.rpc().serve(ClientMessage::CacheQuery {
            job_id: JOB.into(),
            query_id: "q1".into(),
            paths: vec!["/nix/store/abc-malformed".into(), asked.clone()],
            nar_sizes: vec![None, None],
            mode: gradient_wire::types::QueryMode::Normal,
            external: false,
        });
        let reply = tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("answered")
            .expect("reply sent");
        let ServerMessage::CacheStatus { cached, .. } = decode(reply) else {
            panic!("a CacheStatus")
        };
        let cached: Vec<String> = cached.into_iter().map(|hit| hit.path).collect();
        assert_eq!(cached, vec![asked]);
    }
}

#[cfg(test)]
mod running_jobs_tests {
    use super::fixture::{TestSession, decode, pending_build};
    use super::*;
    use gradient_entity::dispatched_job::Model as MDispatchedJob;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_test_support::prelude::*;
    use std::time::Duration;

    fn running(assignment_id: String) -> ClientMessage {
        ClientMessage::RunningJobs {
            assignments: vec![RunningAssignment {
                assignment_id,
                kind: JobKind::Build,
            }],
        }
    }

    #[tokio::test]
    async fn running_jobs_reattach_the_job_to_the_session() {
        let state = test_state(empty_db());
        let (mut session, _sent, _admitted) = TestSession::new(&state).await;
        let shared_build = DerivationBuildId::now_v7();
        let key = format!("build:{shared_build}");
        let a1 = DispatchedJobId::now_v7();
        let (port, _signals) = tokio::sync::mpsc::unbounded_channel();
        session
            .scheduler
            .reattach_worker(
                "w1",
                Default::default(),
                HashSet::new(),
                Arc::new(port),
                vec![Reattached::single(
                    key.clone(),
                    a1,
                    pending_build(shared_build),
                )],
                true,
            )
            .await
            .expect("the worker registers");
        session.scheduler.unregister_worker("w1").await;
        let written = Arc::new(gradient_util::sync::Mutex::new(Vec::new()));
        session.logs = LogLane::spawn(&state.shutdown, {
            let written = Arc::clone(&written);
            move |build, data| {
                let written = Arc::clone(&written);
                async move { written.lock().push((build, data)) }
            }
        });

        let (mut ctx, uploads) = session.split();
        assert!(ctx.handle(running(a1.to_string()), uploads).await);
        let chunk = ClientMessage::LogChunk {
            job_id: key.clone(),
            data: bytes::Bytes::from_static(b"line"),
        };
        assert!(ctx.handle(chunk, uploads).await);

        assert_eq!(session.active.build(&key), Some(shared_build));
        let _ = session.logs.flushed().await.await;
        assert_eq!(*written.lock(), vec![(shared_build, b"line".to_vec())]);
    }

    #[tokio::test]
    async fn an_unknown_assignment_gets_abort_job() {
        let a9 = DispatchedJobId::now_v7();
        let closed = MDispatchedJob {
            id: a9,
            worker_id: "w1".into(),
            job_id: Some("build:x".into()),
            finished_at: Some(gradient_types::now()),
            ..Default::default()
        };
        let state = test_state(
            mock_db()
                .append_query_results([vec![closed]])
                .into_connection(),
        );
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();

        assert!(ctx.handle(running(a9.to_string()), uploads).await);

        let abort = tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("answered")
            .expect("an abort");
        assert!(matches!(
            decode(abort),
            ServerMessage::AbortJob { job_id, .. } if job_id == "build:x"
        ));
    }

    #[tokio::test]
    async fn an_unparsable_assignment_id_is_dropped() {
        let state = test_state(empty_db());
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();

        assert!(ctx.handle(running("nope".into()), uploads).await);

        assert!(sent.try_recv().is_err());
    }
}
