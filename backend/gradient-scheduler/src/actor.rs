/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The scheduler's state as one actor: `WorkerPool` and `JobTracker` are its
//! private state, every mutation is a message, and sessions are reached only
//! through a [`SessionPort`].

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use gradient_score::{InstanceContext, ScoringPolicy};
use gradient_types::ids::{DispatchedJobId, EvaluationId, ProjectId};
use gradient_types::proto::{CandidateScore, GradientCapabilities, JobCandidate, JobKind};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use tracing::{debug, info};

use crate::jobs::{
    Assignment, BoardActiveJob, CandidateDetail, DispatchDecision, JobTracker, PendingJob,
    PendingJobInfo, WorkerCaps,
};
use crate::worker_pool::{WorkerInfo, WorkerPool};

pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// What the scheduler pushes to a session. Every variant is idempotent;
/// `Offers` carries the generation that lets a session coalesce a burst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSignal {
    Offers(u64),
    Reauth,
    Abort {
        job_id: String,
        reason: String,
    },
    Drain,
    /// Tear the session down now. Unlike [`SessionSignal::Drain`] this does not
    /// wait for in-flight jobs: the scheduler has already re-queued them, so the
    /// worker must drop the connection and reconnect rather than keep reporting
    /// into a session the pool no longer knows about.
    Close {
        reason: String,
    },
}

pub trait SessionPort: Send + Sync + 'static {
    fn signal(&self, signal: SessionSignal);
}

impl SessionPort for tokio::sync::mpsc::UnboundedSender<SessionSignal> {
    fn signal(&self, signal: SessionSignal) {
        let _ = self.send(signal);
    }
}

pub struct Registration {
    pub worker: String,
    pub capabilities: GradientCapabilities,
    pub authorized_peers: HashSet<ProjectId>,
    pub session: Arc<dyn SessionPort>,
    pub active: Vec<(String, PendingJob)>,
}

pub struct Registered {
    pub last_seen: Arc<AtomicI64>,
}

#[derive(Debug, Clone)]
pub struct WorkerCapabilities {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub cpu_count: u32,
    pub ram_total_mb: u64,
    pub cpu_core_score: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerMetrics {
    pub cpu_usage_pct: f32,
    pub ram_free_mb: u64,
    pub disk_speed_mbps: Option<f32>,
    pub network_speed_mbps: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub candidates: Vec<JobCandidate>,
    pub generation: u64,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one assignment per RequestJob reply; boxing only relocates the allocation"
)]
pub enum AssignOutcome {
    AtCapacity,
    Nothing,
    Assigned(Assignment),
}

pub struct Released {
    pub job: Option<PendingJob>,
    pub worker_idle: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub workers: usize,
    pub idle_workers: usize,
    pub pending: usize,
    pub active: usize,
    pub pending_builds: u32,
    pub active_builds: u32,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one message per mailbox send; boxing only relocates the allocation"
)]
pub enum SchedulerMsg {
    Register(Registration, RpcReplyPort<Registered>),
    SetWorkerProject {
        worker: String,
        project: ProjectId,
    },
    Unregister {
        worker: String,
        reply: RpcReplyPort<Vec<PendingJob>>,
    },
    IsConnected {
        worker: String,
        reply: RpcReplyPort<bool>,
    },
    AuthorizedFor {
        worker: String,
        project: ProjectId,
        reply: RpcReplyPort<bool>,
    },
    UpdatePeers {
        worker: String,
        peers: HashSet<ProjectId>,
        reply: RpcReplyPort<()>,
    },
    RevokePeers {
        worker: String,
        revoked: HashSet<ProjectId>,
        reply: RpcReplyPort<usize>,
    },
    Reauth {
        worker: String,
    },
    UpdateCapabilities {
        worker: String,
        caps: WorkerCapabilities,
        reply: RpcReplyPort<()>,
    },
    UpdateMetrics {
        worker: String,
        metrics: WorkerMetrics,
        reply: RpcReplyPort<()>,
    },
    MarkDraining {
        worker: String,
        reply: RpcReplyPort<()>,
    },
    Enqueue {
        job_id: String,
        job: PendingJob,
        reply: RpcReplyPort<()>,
    },
    Candidates {
        worker: String,
        only_new: bool,
        reply: RpcReplyPort<Offer>,
    },
    Assign {
        worker: String,
        kind: JobKind,
        instance: Arc<InstanceContext>,
        reply: RpcReplyPort<AssignOutcome>,
    },
    RecordScores {
        worker: String,
        scores: Vec<CandidateScore>,
        reply: RpcReplyPort<()>,
    },
    Rejected {
        worker: String,
        job_id: String,
        reply: RpcReplyPort<()>,
    },
    Release {
        worker: String,
        job_id: String,
        reply: RpcReplyPort<Released>,
    },
    AbortJob {
        worker: String,
        job_id: String,
        reason: String,
        reply: RpcReplyPort<bool>,
    },
    AbortEvaluation {
        evaluation_id: EvaluationId,
        reply: RpcReplyPort<Vec<(String, String)>>,
    },
    RemoveJobs {
        job_ids: Vec<String>,
        reply: RpcReplyPort<()>,
    },
    ActiveJob {
        job_id: String,
        reply: RpcReplyPort<Option<PendingJob>>,
    },
    PendingJob {
        job_id: String,
        reply: RpcReplyPort<Option<PendingJob>>,
    },
    Untracked {
        job_ids: Vec<String>,
        reply: RpcReplyPort<Vec<String>>,
    },
    HasIdleEvalOnlyWorker {
        reply: RpcReplyPort<bool>,
    },
    Workers {
        reply: RpcReplyPort<Vec<WorkerInfo>>,
    },
    WorkerCaps {
        worker: String,
        reply: RpcReplyPort<Option<GradientCapabilities>>,
    },
    StaleWorkers {
        now_ms: i64,
        timeout_ms: i64,
        reply: RpcReplyPort<Vec<String>>,
    },
    Counts {
        reply: RpcReplyPort<Counts>,
    },
    PendingSnapshot {
        reply: RpcReplyPort<Vec<PendingJobInfo>>,
    },
    BoardActiveJobs {
        reply: RpcReplyPort<Vec<BoardActiveJob>>,
    },
    RecentDecisions {
        reply: RpcReplyPort<Vec<DispatchDecision>>,
    },
    CandidateDetail {
        id: DispatchedJobId,
        reply: RpcReplyPort<Option<CandidateDetail>>,
    },
    BumpRescore {
        reply: RpcReplyPort<()>,
    },
    ReOffer,
}

pub struct CoreArgs {
    pub policy: Arc<dyn ScoringPolicy>,
}

pub struct SchedulerCore {
    pool: WorkerPool,
    tracker: JobTracker,
    offers: u64,
    policy: Arc<dyn ScoringPolicy>,
}

impl SchedulerCore {
    fn bump_offers(&mut self) {
        self.offers = self.offers.wrapping_add(1);
        self.pool.signal_active(SessionSignal::Offers(self.offers));
    }

    fn auth_and_caps(&self, worker: &str) -> (Option<HashSet<ProjectId>>, Option<WorkerCaps>) {
        let authorized = self
            .pool
            .peer_auth_for(worker)
            .and_then(|a| a.as_filter())
            .cloned();
        (authorized, self.pool.worker_caps(worker))
    }

    fn candidates(&mut self, worker: &str, only_new: bool) -> Offer {
        let (authorized, caps) = self.auth_and_caps(worker);
        let mut candidates = self
            .tracker
            .candidates_for_worker(authorized.as_ref(), caps.as_ref());
        if only_new && let Some(sent) = self.pool.sent_candidates_for(worker) {
            candidates.retain(|c| !sent.contains(&c.job_id));
        }
        let ids: Vec<String> = candidates.iter().map(|c| c.job_id.clone()).collect();
        self.pool.mark_candidates_sent(worker, &ids);
        Offer {
            candidates,
            generation: self.offers,
        }
    }

    fn assign(
        &mut self,
        worker: &str,
        kind: &JobKind,
        instance: &InstanceContext,
    ) -> AssignOutcome {
        if !self.pool.has_capacity(worker, kind) {
            debug!(%worker, ?kind, "RequestJob ignored - worker at capacity");
            return AssignOutcome::AtCapacity;
        }
        let (authorized, caps) = self.auth_and_caps(worker);
        let policy = Arc::clone(&self.policy);
        match self.tracker.take_best_of_kind(
            worker,
            authorized.as_ref(),
            caps.as_ref(),
            kind,
            &*policy,
            instance,
        ) {
            Some(assignment) => {
                self.pool.assign_job(worker, &assignment.job_id);
                AssignOutcome::Assigned(assignment)
            }
            None => AssignOutcome::Nothing,
        }
    }
}

pub struct CoreActor;

impl Actor for CoreActor {
    type Msg = SchedulerMsg;
    type State = SchedulerCore;
    type Arguments = CoreArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(SchedulerCore {
            pool: WorkerPool::new(),
            tracker: JobTracker::new(),
            offers: 0,
            policy: args.policy,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        core: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SchedulerMsg::Register(reg, reply) => {
                let last_seen = core.pool.register(
                    reg.worker.clone(),
                    reg.capabilities,
                    reg.authorized_peers,
                    reg.session,
                );
                for (job_id, job) in reg.active {
                    core.tracker
                        .restore_active(&reg.worker, job_id.clone(), job);
                    core.pool.assign_job(&reg.worker, &job_id);
                }
                info!(worker = %reg.worker, "worker registered");
                let _ = reply.send(Registered { last_seen });
            }
            SchedulerMsg::SetWorkerProject { worker, project } => {
                core.pool.set_worker_project(&worker, project);
            }
            SchedulerMsg::Unregister { worker, reply } => {
                let orphaned = core.pool.unregister(&worker);
                let requeued = core.tracker.worker_disconnected(&worker);
                let total = orphaned.len() + requeued.len();
                if total > 0 {
                    info!(%worker, orphaned_jobs = total, "worker disconnected; jobs re-queued");
                }
                let _ = reply.send(requeued);
            }
            SchedulerMsg::IsConnected { worker, reply } => {
                let _ = reply.send(core.pool.is_connected(&worker));
            }
            SchedulerMsg::AuthorizedFor {
                worker,
                project,
                reply,
            } => {
                let ok = core
                    .pool
                    .peer_auth_for(&worker)
                    .map(|a| a.contains(&project))
                    .unwrap_or(false);
                let _ = reply.send(ok);
            }
            SchedulerMsg::UpdatePeers {
                worker,
                peers,
                reply,
            } => {
                core.pool.update_authorized_peers(&worker, peers);
                let _ = reply.send(());
            }
            SchedulerMsg::RevokePeers {
                worker,
                revoked,
                reply,
            } => {
                let job_ids = core.tracker.drain_peer_jobs_on_worker(&worker, &revoked);
                for job_id in &job_ids {
                    core.pool.send_abort(
                        &worker,
                        job_id.clone(),
                        "project deactivated worker".to_owned(),
                    );
                }
                if !job_ids.is_empty() {
                    core.bump_offers();
                }
                let _ = reply.send(job_ids.len());
            }
            SchedulerMsg::Reauth { worker } => core.pool.request_reauth(&worker),
            SchedulerMsg::UpdateCapabilities {
                worker,
                caps,
                reply,
            } => {
                core.pool.update_capabilities(
                    &worker,
                    caps.architectures,
                    caps.system_features,
                    caps.max_concurrent_builds,
                    caps.cpu_count,
                    caps.ram_total_mb,
                    caps.cpu_core_score,
                );
                let _ = reply.send(());
            }
            SchedulerMsg::UpdateMetrics {
                worker,
                metrics,
                reply,
            } => {
                core.pool.update_metrics(
                    &worker,
                    metrics.cpu_usage_pct,
                    metrics.ram_free_mb,
                    metrics.disk_speed_mbps,
                    metrics.network_speed_mbps,
                );
                let _ = reply.send(());
            }
            SchedulerMsg::MarkDraining { worker, reply } => {
                core.pool.mark_draining(&worker);
                let _ = reply.send(());
            }
            SchedulerMsg::Enqueue { job_id, job, reply } => {
                let is_build = matches!(job, PendingJob::Build(_));
                core.tracker.add_pending(job_id.clone(), job);
                if is_build {
                    core.pool.remove_sent_candidate(&job_id);
                }
                core.bump_offers();
                let _ = reply.send(());
            }
            SchedulerMsg::Candidates {
                worker,
                only_new,
                reply,
            } => {
                let _ = reply.send(core.candidates(&worker, only_new));
            }
            SchedulerMsg::Assign {
                worker,
                kind,
                instance,
                reply,
            } => {
                let _ = reply.send(core.assign(&worker, &kind, &instance));
            }
            SchedulerMsg::RecordScores {
                worker,
                scores,
                reply,
            } => {
                core.tracker.record_scores(&worker, scores);
                let _ = reply.send(());
            }
            SchedulerMsg::Rejected {
                worker,
                job_id,
                reply,
            } => {
                core.pool.release_job(&worker, &job_id);
                core.tracker.release_to_pending(&job_id);
                core.pool.remove_sent_candidate(&job_id);
                info!(%worker, %job_id, "job rejected; re-queued");
                let _ = reply.send(());
            }
            SchedulerMsg::Release {
                worker,
                job_id,
                reply,
            } => {
                let worker_idle = core.pool.release_job(&worker, &job_id);
                let job = core.tracker.remove_active(&job_id);
                let _ = reply.send(Released { job, worker_idle });
            }
            SchedulerMsg::AbortJob {
                worker,
                job_id,
                reason,
                reply,
            } => {
                let _ = reply.send(core.pool.send_abort(&worker, job_id, reason));
            }
            SchedulerMsg::AbortEvaluation {
                evaluation_id,
                reply,
            } => {
                let to_abort: Vec<(String, String)> = core
                    .tracker
                    .active_jobs()
                    .filter(|(_, _, job)| job.evaluation_id() == evaluation_id)
                    .map(|(job_id, worker, _)| (worker.to_owned(), job_id.to_owned()))
                    .collect();
                for (worker, job_id) in &to_abort {
                    core.pool
                        .send_abort(worker, job_id.clone(), "evaluation aborted".to_owned());
                }
                core.tracker.remove_pending_for_evaluation(evaluation_id);
                let _ = reply.send(to_abort);
            }
            SchedulerMsg::RemoveJobs { job_ids, reply } => {
                for id in &job_ids {
                    core.tracker.remove_job(id);
                }
                let _ = reply.send(());
            }
            SchedulerMsg::ActiveJob { job_id, reply } => {
                let _ = reply.send(core.tracker.active_job(&job_id).cloned());
            }
            SchedulerMsg::PendingJob { job_id, reply } => {
                let _ = reply.send(core.tracker.pending_job(&job_id).cloned());
            }
            SchedulerMsg::Untracked { job_ids, reply } => {
                let unknown = job_ids
                    .into_iter()
                    .filter(|id| !core.tracker.contains_job(id))
                    .collect();
                let _ = reply.send(unknown);
            }
            SchedulerMsg::HasIdleEvalOnlyWorker { reply } => {
                let _ = reply.send(core.pool.has_idle_eval_only_worker());
            }
            SchedulerMsg::Workers { reply } => {
                let _ = reply.send(core.pool.all_workers());
            }
            SchedulerMsg::WorkerCaps { worker, reply } => {
                let _ = reply.send(core.pool.gradient_caps_for(&worker));
            }
            SchedulerMsg::StaleWorkers {
                now_ms,
                timeout_ms,
                reply,
            } => {
                let _ = reply.send(core.pool.stale_worker_ids(now_ms, timeout_ms));
            }
            SchedulerMsg::Counts { reply } => {
                let (workers, idle_workers) = core.pool.worker_counts();
                let (active_builds, pending_builds) = core.tracker.instance_counts();
                let _ = reply.send(Counts {
                    workers: workers as usize,
                    idle_workers: idle_workers as usize,
                    pending: core.tracker.pending_count(),
                    active: core.tracker.active_count(),
                    pending_builds,
                    active_builds,
                });
            }
            SchedulerMsg::PendingSnapshot { reply } => {
                let _ = reply.send(core.tracker.pending_snapshot());
            }
            SchedulerMsg::BoardActiveJobs { reply } => {
                let _ = reply.send(core.tracker.board_active_jobs());
            }
            SchedulerMsg::RecentDecisions { reply } => {
                let _ = reply.send(core.tracker.recent_decisions());
            }
            SchedulerMsg::CandidateDetail { id, reply } => {
                let _ = reply.send(core.tracker.candidate_detail(id));
            }
            SchedulerMsg::BumpRescore { reply } => {
                core.tracker.bump_rescore_counts();
                let _ = reply.send(());
            }
            SchedulerMsg::ReOffer => {
                if core.tracker.has_pending() {
                    core.bump_offers();
                }
            }
        }
        Ok(())
    }
}
