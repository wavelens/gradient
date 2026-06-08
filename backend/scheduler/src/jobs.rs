/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pending and active job tracking.

use std::collections::{HashMap, HashSet};

use gradient_core::types::ids::{
    BuildId, CommitId, DerivationId, EvaluationId, OrganizationId, ProjectId,
};
use gradient_core::types::proto::{
    BuildJob, CandidateScore, FlakeJob, FlakeSource, FlakeTask, Job, JobCandidate, JobKind,
    RequiredPath,
};

use score::{JobContext, LazyProviders, ScoredJob, ScoringPolicy, WorkerContext};

#[derive(Debug, Clone)]
pub struct PendingEvalJob {
    pub evaluation_id: EvaluationId,
    pub project_id: Option<ProjectId>,
    /// Peer (org/cache/proxy) that owns this job. Workers must be authorized
    /// for this peer to receive the job offer.
    pub peer_id: OrganizationId,
    pub commit_id: CommitId,
    pub repository: String,
    pub job: FlakeJob,
    pub required_paths: Vec<RequiredPath>,
    /// `evaluation.updated_at` at the time this job was dispatched.
    /// Used by the scoring policy to prefer evaluations that have waited longer.
    pub queued_at: chrono::NaiveDateTime,
    /// When the job became dispatchable. Evals have no dependencies, so this
    /// equals `queued_at`.
    pub ready_at: chrono::NaiveDateTime,
    /// Number of dispatch ticks this job has waited while pending. Bumped once
    /// per dispatch loop and fed into the scoring policy's rescore-wait rule.
    pub rescore_count: u32,
}

impl PendingEvalJob {
    pub fn cached_followup(&self, store_path: String) -> PendingEvalJob {
        let mut follow = self.clone();
        follow.job.tasks = vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations];
        follow.job.source = FlakeSource::Cached { store_path: store_path.clone() };
        follow.required_paths = vec![RequiredPath { path: store_path, cache_info: None }];
        follow
    }
}

#[derive(Debug, Clone)]
pub struct PendingBuildJob {
    pub build_id: BuildId,
    pub evaluation_id: EvaluationId,
    /// Peer (org/cache/proxy) that owns this job.
    pub peer_id: OrganizationId,
    pub job: BuildJob,
    pub required_paths: Vec<RequiredPath>,
    /// Nix system string the build's target derivation must run on
    /// (e.g. `"x86_64-linux"`, `"aarch64-linux"`, `"builtin"`).
    pub architecture: String,
    /// Nix system features the build needs (e.g. `["kvm", "big-parallel"]`).
    pub required_features: Vec<String>,
    /// Number of direct derivation dependencies (inputs) this build has.
    /// Used by the scoring policy to prefer builds that unblock more work.
    pub dependency_count: u32,
    /// Total transitive output NAR size of the build's closure, when known.
    /// Fed into the scoring policy's resource-aware rules.
    pub closure_size: Option<i64>,
    /// `derivation.prefer_local_build`: this build should run on the dispatching
    /// peer's local workers rather than be offloaded.
    pub prefer_local_build: bool,
    /// `derivation.is_fixed_output`: a fixed-output derivation fetches from the
    /// network, so scoring prefers faster-network workers.
    pub is_fixed_output: bool,
    /// Historical resource-usage prediction for this build's derivation,
    /// preloaded once per dispatch round and consumed by scoring rules.
    pub history: score::HistoryPrediction,
    /// `build.updated_at` at the time this job was dispatched to the tracker.
    /// Used by the scoring policy to prefer builds that have waited longer.
    pub queued_at: chrono::NaiveDateTime,
    /// When the build became dispatchable (all dependencies satisfied). A build
    /// is enqueued only once ready, so this is "now" at enqueue time.
    pub ready_at: chrono::NaiveDateTime,
    /// Number of dispatch ticks this job has waited while pending. Bumped once
    /// per dispatch loop and fed into the scoring policy's rescore-wait rule.
    pub rescore_count: u32,
    /// `derivation.pname`, surfaced for the serialized dispatch view.
    pub pname: Option<String>,
}

/// A connected worker's capabilities, used to gate which jobs are eligible
/// for assignment: the `fetch` gradient capability plus the Nix architectures
/// and system features it can build for.
#[derive(Debug, Clone, Default)]
pub struct WorkerCaps {
    /// Worker can fetch flake sources from a repository. Required for any
    /// FlakeJob carrying a `FetchFlake` task, since the server only sends SSH
    /// credentials to fetch-capable workers.
    pub fetch: bool,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    /// Full set of advertised gradient capabilities, surfaced on the dispatch view.
    pub capabilities: gradient_core::types::proto::GradientCapabilities,
    /// Live resource view of the worker, fed into resource-aware scoring rules.
    pub metrics: Option<score::WorkerMetricsView>,
}

impl WorkerCaps {
    /// Returns true when this worker can execute a build with the given
    /// `architecture` and `required_features`. `"builtin"` derivations
    /// (`builtin:fetchurl` etc.) run on any architecture.
    pub fn can_build(&self, architecture: &str, required_features: &[String]) -> bool {
        let arch_ok =
            architecture == "builtin" || self.architectures.iter().any(|a| a == architecture);
        let features_ok = required_features
            .iter()
            .all(|f| self.system_features.iter().any(|sf| sf == f));
        arch_ok && features_ok
    }
}

#[derive(Debug, Clone)]
pub enum PendingJob {
    Eval(PendingEvalJob),
    Build(PendingBuildJob),
}

impl PendingJob {
    pub fn required_paths(&self) -> &[RequiredPath] {
        match self {
            PendingJob::Eval(j) => &j.required_paths,
            PendingJob::Build(j) => &j.required_paths,
        }
    }

    pub fn peer_id(&self) -> OrganizationId {
        match self {
            PendingJob::Eval(j) => j.peer_id,
            PendingJob::Build(j) => j.peer_id,
        }
    }

    pub fn as_candidate(&self, job_id: &str) -> JobCandidate {
        let drv_paths = match self {
            PendingJob::Build(j) => j.job.builds.iter().map(|t| t.drv_path.clone()).collect(),
            PendingJob::Eval(_) => vec![],
        };
        JobCandidate {
            job_id: job_id.to_owned(),
            required_paths: self.required_paths().to_vec(),
            drv_paths,
        }
    }

    fn into_job(self) -> Job {
        match self {
            PendingJob::Eval(j) => Job::Flake(j.job),
            PendingJob::Build(j) => Job::Build(j.job),
        }
    }

    pub fn evaluation_id(&self) -> EvaluationId {
        match self {
            PendingJob::Eval(j) => j.evaluation_id,
            PendingJob::Build(j) => j.evaluation_id,
        }
    }

    pub fn dependency_count(&self) -> u32 {
        match self {
            PendingJob::Build(j) => j.dependency_count,
            PendingJob::Eval(_) => 0,
        }
    }

    pub fn queued_at(&self) -> chrono::NaiveDateTime {
        match self {
            PendingJob::Build(j) => j.queued_at,
            PendingJob::Eval(j) => j.queued_at,
        }
    }

    pub fn ready_at(&self) -> chrono::NaiveDateTime {
        match self {
            PendingJob::Build(j) => j.ready_at,
            PendingJob::Eval(j) => j.ready_at,
        }
    }

    pub fn rescore_count(&self) -> u32 {
        match self {
            PendingJob::Build(j) => j.rescore_count,
            PendingJob::Eval(j) => j.rescore_count,
        }
    }

    pub fn set_rescore_count(&mut self, n: u32) {
        match self {
            PendingJob::Build(j) => j.rescore_count = n,
            PendingJob::Eval(j) => j.rescore_count = n,
        }
    }
}

pub struct Assignment {
    pub job_id: String,
    pub job: Job,
    /// Organization UUID that owns this job - used for credential lookup.
    pub peer_id: OrganizationId,
    /// Scoring/context snapshot for the winning job, persisted best-effort by
    /// the caller into `dispatched_job`. `None` outside the scored path.
    pub dispatch_record: Option<DispatchRecord>,
}

/// Owned snapshot of a dispatch decision for the `dispatched_job` table.
pub struct DispatchRecord {
    pub kind: i16,
    pub build_id: Option<BuildId>,
    pub evaluation_id: EvaluationId,
    pub organization: OrganizationId,
    pub project: Option<ProjectId>,
    pub derivation: Option<DerivationId>,
    pub score: f64,
    pub queued_at: chrono::NaiveDateTime,
    pub ready_at: chrono::NaiveDateTime,
    pub score_breakdown: serde_json::Value,
    pub worker_context: serde_json::Value,
    pub job_context: serde_json::Value,
}

/// Returns true when the worker can execute `job`: a flake job that fetches
/// from a repository (carries `FetchFlake`) needs the `fetch` capability; a
/// build job needs matching architecture/features. If `caps` is `None`,
/// capability checks are skipped (used for tests / open mode).
fn job_eligible_for_caps(job: &PendingJob, caps: Option<&WorkerCaps>) -> bool {
    match (job, caps) {
        // No capability info known → don't block (legacy behaviour for callers
        // that don't supply caps, e.g. unit tests for unrelated logic).
        (_, None) => true,
        // A repository-source flake job clones over the network and so requires
        // `fetch`; eval-only follow-up jobs (cached source) run on any worker.
        (PendingJob::Eval(j), Some(c)) => c.fetch || !j.job.tasks.contains(&FlakeTask::FetchFlake),
        (PendingJob::Build(j), Some(c)) => c.can_build(&j.architecture, &j.required_features),
    }
}

/// True when a flake job's only task is `FetchFlake` — a split-mode fetch-only
/// job whose completion enqueues a cached eval follow-up rather than finalizing.
pub fn is_fetch_only(job: &FlakeJob) -> bool {
    job.tasks.as_slice() == [FlakeTask::FetchFlake]
}

/// Per-job score submitted by a worker after checking its local store.
#[derive(Debug, Clone, Default)]
pub struct WorkerJobScore {
    /// Number of required store paths not present in the worker's store.
    pub missing_count: u32,
    /// Total uncompressed NAR size (bytes) of the missing paths.
    pub missing_nar_size: u64,
}

#[derive(Debug, Clone)]
pub struct PendingJobInfo {
    pub kind: i16,
    pub evaluation_id: EvaluationId,
    pub build_id: Option<BuildId>,
    pub organization: OrganizationId,
    pub queued_at: chrono::NaiveDateTime,
    pub dependency_count: u32,
}

#[derive(Debug, Default)]
pub struct JobTracker {
    pending: HashMap<String, PendingJob>,
    /// Per-worker, per-job scores: `worker_id → job_id → score`.
    scores: HashMap<String, HashMap<String, WorkerJobScore>>,
    active: HashMap<String, (String, PendingJob)>,
}

impl JobTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pending(&mut self, job_id: String, job: PendingJob) -> JobCandidate {
        let candidate = job.as_candidate(&job_id);
        // Idempotent under the tracker write lock: two concurrent
        // `dispatch_ready_builds` passes can both clear the `contains_job`
        // filter before either enqueues, so a job already pending or in-flight
        // (active) must not be re-queued - otherwise the same build is
        // dispatched to the worker twice and the duplicate fails the eval.
        if self.pending.contains_key(&job_id) || self.active.contains_key(&job_id) {
            return candidate;
        }
        self.pending.insert(job_id, job);
        candidate
    }

    /// Returns all pending job candidates that the worker is authorized to receive
    /// AND can execute. `caps` filters build jobs to those matching the worker's
    /// architectures and system features, and fetch flake jobs to fetch-capable
    /// workers. Pass `None` for `authorized` to disable peer filtering (open mode).
    /// Pass `None` for `caps` to disable capability filtering.
    pub fn candidates_for_worker(
        &self,
        authorized: Option<&HashSet<OrganizationId>>,
        caps: Option<&WorkerCaps>,
    ) -> Vec<JobCandidate> {
        self.pending
            .iter()
            .filter(|(_, job)| {
                authorized.is_none_or(|peers| peers.contains(&job.peer_id()))
                    && job_eligible_for_caps(job, caps)
            })
            .map(|(id, job)| job.as_candidate(id))
            .collect()
    }

    /// Record scores from a worker without assigning anything. The server only
    /// assigns jobs in response to an explicit `RequestJob` - scores just
    /// inform which candidate to pick at that point.
    pub fn record_scores(&mut self, peer_id: &str, scores: Vec<CandidateScore>) {
        let worker_scores = self.scores.entry(peer_id.to_owned()).or_default();
        for score in scores {
            worker_scores.insert(
                score.job_id.clone(),
                WorkerJobScore {
                    missing_count: score.missing_count,
                    missing_nar_size: score.missing_nar_size,
                },
            );
        }
    }

    /// Assign the best pending job matching `kind` for `peer_id`.
    ///
    /// Each eligible candidate is scored by `policy`.  The job with the
    /// highest total score is assigned.  When multiple jobs tie, the one with
    /// the lexicographically smallest `job_id` is chosen for determinism.
    ///
    /// This is the ONLY assignment path - the server never assigns without
    /// an explicit `RequestJob` from the worker.
    pub fn take_best_of_kind(
        &mut self,
        peer_id: &str,
        authorized: Option<&HashSet<OrganizationId>>,
        caps: Option<&WorkerCaps>,
        kind: &JobKind,
        policy: &dyn ScoringPolicy,
        instance: &score::InstanceContext,
    ) -> Option<Assignment> {
        let worker_scores = self.scores.get(peer_id);

        let worker_ctx = caps.map(|c| WorkerContext {
            architectures: &c.architectures,
            system_features: &c.system_features,
            fetch: c.fetch,
            metrics: c.metrics,
        });
        let empty_archs: Vec<String> = vec![];
        let empty_feats: Vec<String> = vec![];
        let fallback_ctx = WorkerContext {
            architectures: &empty_archs,
            system_features: &empty_feats,
            fetch: false,
            metrics: None,
        };
        let worker_ctx = worker_ctx.as_ref().unwrap_or(&fallback_ctx);

        let mut org_work: HashMap<OrganizationId, f64> = HashMap::new();
        let mut total_work: f64 = 0.0;
        for (_, job) in self.active.values() {
            if let PendingJob::Build(b) = job {
                let w = if b.history.build_time_ms > 0 {
                    b.history.build_time_ms as f64
                } else {
                    // no per-build history: weight by instance avg, half for prefer-local (cheaper) builds
                    (if b.prefer_local_build { 0.5 } else { 1.0 }) * instance.build_time_ms.w1h
                };
                *org_work.entry(b.peer_id).or_default() += w;
                total_work += w;
            }
        }
        let org_work_share = |peer: OrganizationId| -> Option<f32> {
            (total_work > 0.0).then(|| (org_work.get(&peer).copied().unwrap_or(0.0) / total_work) as f32)
        };

        let score_of = |id: &str, job: &PendingJob| -> f64 {
            let s = worker_scores.and_then(|ws| ws.get(id));
            let closure_size = match job {
                PendingJob::Build(b) => b.closure_size,
                PendingJob::Eval(_) => None,
            };
            let history = match job {
                PendingJob::Build(b) => b.history,
                PendingJob::Eval(_) => score::HistoryPrediction::default(),
            };
            let closure = move || closure_size;
            let history_provider = move || history;
            let scored = match job {
                PendingJob::Eval(e) => ScoredJob::new_eval(
                    id,
                    job.peer_id(),
                    e.job.tasks.contains(&FlakeTask::FetchFlake),
                ),
                PendingJob::Build(b) => ScoredJob::new_build(
                    id,
                    job.peer_id(),
                    b.architecture.as_str(),
                    b.prefer_local_build,
                    b.is_fixed_output,
                    None,
                    LazyProviders { closure_size: &closure, history: &history_provider },
                ),
            };
            let ctx = JobContext {
                job: &scored,
                missing_count: s.map(|s| s.missing_count),
                missing_nar_size: s.map(|s| s.missing_nar_size),
                dependency_count: job.dependency_count(),
                queued_at: job.queued_at(),
                ready_at: job.ready_at(),
                org_work_share: org_work_share(job.peer_id()),
                rescore_count: job.rescore_count(),
            };
            policy.score(&ctx, worker_ctx, instance)
        };

        let best = self
            .pending
            .iter()
            .filter(|(_, j)| {
                authorized.is_none_or(|peers| peers.contains(&j.peer_id()))
                    && matches!(
                        (kind, j),
                        (JobKind::Flake, PendingJob::Eval(_))
                            | (JobKind::Build, PendingJob::Build(_))
                    )
                    && job_eligible_for_caps(j, caps)
            })
            .map(|(id, job)| (id, score_of(id, job)))
            .max_by(|(id_a, sa), (id_b, sb)| {
                sa.partial_cmp(sb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Tie-break by job_id for determinism.
                    .then_with(|| id_b.cmp(id_a))
            });

        // A negative best total means dispatching now is worse than idling this
        // round (e.g. a build still awaiting candidate scores); the worker waits.
        let job_id = match best {
            Some((id, s)) if s >= 0.0 => id.clone(),
            _ => return None,
        };

        // Recompute the winner's score with the per-rule breakdown, captured for
        // the dispatched_job row. Owned snapshots so the borrow ends before the
        // mutable assign_pending below.
        let dispatch_record = self.pending.get(&job_id).map(|job| {
            let s = worker_scores.and_then(|ws| ws.get(job_id.as_str()));
            let closure_size = match job {
                PendingJob::Build(b) => b.closure_size,
                PendingJob::Eval(_) => None,
            };
            let history = match job {
                PendingJob::Build(b) => b.history,
                PendingJob::Eval(_) => score::HistoryPrediction::default(),
            };
            let closure = move || closure_size;
            let history_provider = move || history;
            let scored = match job {
                PendingJob::Eval(e) => ScoredJob::new_eval(
                    job_id.as_str(),
                    job.peer_id(),
                    e.job.tasks.contains(&FlakeTask::FetchFlake),
                ),
                PendingJob::Build(b) => ScoredJob::new_build(
                    job_id.as_str(),
                    job.peer_id(),
                    b.architecture.as_str(),
                    b.prefer_local_build,
                    b.is_fixed_output,
                    None,
                    LazyProviders { closure_size: &closure, history: &history_provider },
                ),
            };
            let ctx = JobContext {
                job: &scored,
                missing_count: s.map(|s| s.missing_count),
                missing_nar_size: s.map(|s| s.missing_nar_size),
                dependency_count: job.dependency_count(),
                queued_at: job.queued_at(),
                ready_at: job.ready_at(),
                org_work_share: org_work_share(job.peer_id()),
                rescore_count: job.rescore_count(),
            };
            let breakdown = policy.score_detailed(&ctx, worker_ctx, instance);
            let (kind_disc, build_id, project) = match job {
                PendingJob::Build(b) => (1i16, Some(b.build_id), None),
                PendingJob::Eval(e) => (0i16, None, e.project_id),
            };
            DispatchRecord {
                kind: kind_disc,
                build_id,
                evaluation_id: job.evaluation_id(),
                organization: job.peer_id(),
                project,
                derivation: None,
                score: breakdown.total,
                queued_at: job.queued_at(),
                ready_at: ctx.ready_at,
                score_breakdown: serde_json::to_value(&breakdown)
                    .unwrap_or(serde_json::Value::Null),
                worker_context: serde_json::to_value(crate::views::WorkerContextView::new(
                    worker_ctx,
                    caps.map(|c| c.capabilities.clone()).unwrap_or_default(),
                ))
                .unwrap_or(serde_json::Value::Null),
                job_context: serde_json::to_value(crate::views::JobContextView::new(&ctx, job))
                    .unwrap_or(serde_json::Value::Null),
            }
        });

        let mut assignment = self.assign_pending(peer_id, &job_id)?;
        assignment.dispatch_record = dispatch_record;
        Some(assignment)
    }

    fn assign_pending(&mut self, peer_id: &str, job_id: &str) -> Option<Assignment> {
        let job = self.pending.remove(job_id)?;
        if let Some(ws) = self.scores.get_mut(peer_id) {
            ws.remove(job_id);
        }
        let assignment = Assignment {
            job_id: job_id.to_owned(),
            job: job.clone().into_job(),
            peer_id: job.peer_id(),
            dispatch_record: None,
        };
        self.active
            .insert(job_id.to_owned(), (peer_id.to_owned(), job));
        Some(assignment)
    }

    pub fn release_to_pending(&mut self, job_id: &str) {
        if let Some((_, job)) = self.active.remove(job_id) {
            self.pending.insert(job_id.to_owned(), job);
        }
    }

    pub fn remove_active(&mut self, job_id: &str) -> Option<PendingJob> {
        self.active.remove(job_id).map(|(_, j)| j)
    }

    pub fn active_job(&self, job_id: &str) -> Option<&PendingJob> {
        self.active.get(job_id).map(|(_, j)| j)
    }

    pub fn pending_job(&self, job_id: &str) -> Option<&PendingJob> {
        self.pending.get(job_id)
    }

    /// Move all active jobs assigned to `worker_id` that belong to any of
    /// `revoked_peers` back to the pending queue.  Returns the job IDs so the
    /// caller can send `AbortJob` messages to the worker.
    pub fn drain_peer_jobs_on_worker(
        &mut self,
        worker_id: &str,
        revoked_peers: &HashSet<OrganizationId>,
    ) -> Vec<String> {
        let to_requeue: Vec<String> = self
            .active
            .iter()
            .filter(|(_, (w, job))| w == worker_id && revoked_peers.contains(&job.peer_id()))
            .map(|(id, _)| id.clone())
            .collect();
        for job_id in &to_requeue {
            if let Some((_, job)) = self.active.remove(job_id) {
                self.pending.insert(job_id.clone(), job);
            }
        }
        to_requeue
    }

    pub fn worker_disconnected(&mut self, peer_id: &str) -> Vec<String> {
        self.scores.remove(peer_id);
        let orphaned: Vec<String> = self
            .active
            .iter()
            .filter(|(_, (w, _))| w == peer_id)
            .map(|(id, _)| id.clone())
            .collect();
        for job_id in &orphaned {
            if let Some((_, job)) = self.active.remove(job_id) {
                self.pending.insert(job_id.clone(), job);
            }
        }
        orphaned
    }

    pub fn contains_job(&self, job_id: &str) -> bool {
        self.pending.contains_key(job_id) || self.active.contains_key(job_id)
    }

    pub fn remove_job(&mut self, job_id: &str) {
        self.pending.remove(job_id);
        self.active.remove(job_id);
    }

    /// Iterate over active jobs: yields `(job_id, worker_id, &PendingJob)`.
    pub fn active_jobs(&self) -> impl Iterator<Item = (&str, &str, &PendingJob)> {
        self.active
            .iter()
            .map(|(job_id, (worker_id, job))| (job_id.as_str(), worker_id.as_str(), job))
    }

    /// Remove all pending (unassigned) jobs belonging to a given evaluation.
    pub fn remove_pending_for_evaluation(&mut self, evaluation_id: EvaluationId) {
        self.pending
            .retain(|_, job| job.evaluation_id() != evaluation_id);
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn pending_snapshot(&self) -> Vec<PendingJobInfo> {
        self.pending
            .values()
            .map(|job| {
                let (kind, build_id) = match job {
                    PendingJob::Build(b) => (1i16, Some(b.build_id)),
                    PendingJob::Eval(_) => (0i16, None),
                };
                PendingJobInfo {
                    kind,
                    evaluation_id: job.evaluation_id(),
                    build_id,
                    organization: job.peer_id(),
                    queued_at: job.queued_at(),
                    dependency_count: job.dependency_count(),
                }
            })
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// `(active_builds, pending_builds)` — build jobs in flight and waiting,
    /// fed into the windowed instance snapshot.
    pub fn instance_counts(&self) -> (u32, u32) {
        let active = self
            .active
            .values()
            .filter(|(_, j)| matches!(j, PendingJob::Build(_)))
            .count() as u32;
        let pending = self
            .pending
            .values()
            .filter(|j| matches!(j, PendingJob::Build(_)))
            .count() as u32;
        (active, pending)
    }

    /// Increment every pending job's `rescore_count`. Called once per build
    /// dispatch tick so long-waiting jobs accrue a rescore-wait bonus.
    pub fn bump_rescore_counts(&mut self) {
        for j in self.pending.values_mut() {
            j.set_rescore_count(j.rescore_count() + 1);
        }
    }

    #[cfg(test)]
    pub fn rescore_count_of(&self, job_id: &str) -> u32 {
        self.pending.get(job_id).map(|j| j.rescore_count()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::types::proto::{BuildJob, BuildTask, FlakeJob, FlakeSource, FlakeTask};

    fn eval_job(peer: OrganizationId) -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: EvaluationId::now_v7(),
            project_id: None,
            peer_id: peer,
            commit_id: CommitId::now_v7(),
            repository: "https://example.com/repo".into(),
            job: FlakeJob {
                tasks: vec![FlakeTask::EvaluateDerivations],
                source: FlakeSource::Repository {
                    url: "https://example.com/repo".into(),
                    commit: "abc123".into(),
                },
                wildcards: vec!["*".into()],
                timeout_secs: None,
                input_overrides: vec![],
            },
            required_paths: vec![],
            queued_at: gradient_core::types::now(),
            ready_at: gradient_core::types::now(),
            rescore_count: 0,
        })
    }

    fn fetch_eval_job(peer: OrganizationId) -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: EvaluationId::now_v7(),
            project_id: None,
            peer_id: peer,
            commit_id: CommitId::now_v7(),
            repository: "git+ssh://git@example.com/repo".into(),
            job: FlakeJob {
                tasks: vec![
                    FlakeTask::FetchFlake,
                    FlakeTask::EvaluateFlake,
                    FlakeTask::EvaluateDerivations,
                ],
                source: FlakeSource::Repository {
                    url: "git+ssh://git@example.com/repo".into(),
                    commit: "abc123".into(),
                },
                wildcards: vec!["*".into()],
                timeout_secs: None,
                input_overrides: vec![],
            },
            required_paths: vec![],
            queued_at: gradient_core::types::now(),
            ready_at: gradient_core::types::now(),
            rescore_count: 0,
        })
    }

    fn build_job(peer: OrganizationId, required: Vec<RequiredPath>) -> PendingJob {
        build_job_arch(peer, required, "x86_64-linux", vec![])
    }

    fn build_job_arch(
        peer: OrganizationId,
        required: Vec<RequiredPath>,
        architecture: &str,
        required_features: Vec<String>,
    ) -> PendingJob {
        PendingJob::Build(PendingBuildJob {
            build_id: BuildId::now_v7(),
            evaluation_id: EvaluationId::now_v7(),
            peer_id: peer,
            job: BuildJob {
                builds: vec![BuildTask {
                    build_id: BuildId::now_v7().to_string(),
                    drv_path: "/nix/store/abc.drv".into(),
                    external_cached: false,
                    timeout_secs: None,
                    max_silent_secs: None,
                }],
            },
            required_paths: required,
            architecture: architecture.into(),
            required_features,
            dependency_count: 0,
            closure_size: None,
            prefer_local_build: false,
            is_fixed_output: false,
            history: score::HistoryPrediction::default(),
            queued_at: gradient_core::types::now(),
            ready_at: gradient_core::types::now(),
            rescore_count: 0,
            pname: None,
        })
    }

    #[test]
    fn can_build_multi_arch_worker_accepts_one_of_many() {
        // Worker with multiple architectures must accept a build whose target
        // matches ANY (not ALL) of its listed architectures. Guards against
        // `.any()` → `.all()` in the capability check.
        let caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into(), "aarch64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        assert!(caps.can_build("x86_64-linux", &[]));
        assert!(caps.can_build("aarch64-linux", &[]));
        assert!(!caps.can_build("riscv64-linux", &[]));
    }

    #[test]
    fn can_build_requires_all_features() {
        // Worker must provide EVERY required feature (not just one). Guards
        // against `.all()` → `.any()` in the feature check.
        let caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec!["kvm".into()],
            ..Default::default()
        };
        assert!(caps.can_build("x86_64-linux", &["kvm".into()]));
        // kvm is provided but big-parallel is not → must reject.
        assert!(!caps.can_build("x86_64-linux", &["kvm".into(), "big-parallel".into()],));
    }

    #[test]
    fn add_pending_does_not_requeue_active_job() {
        // Regression: two concurrent dispatch passes can both pass the
        // `contains_job` filter before either enqueues. Once a job is assigned
        // (active), re-adding the same id must not put it back in pending, or it
        // gets dispatched to the worker a second time and the duplicate build is
        // aborted by the daemon - failing the whole evaluation.
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("build:1".into(), build_job(peer, vec![]));
        assert!(
            tracker.assign_pending("worker", "build:1").is_some(),
            "job should assign"
        );
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);

        tracker.add_pending("build:1".into(), build_job(peer, vec![]));
        assert_eq!(tracker.pending_count(), 0, "active job must not be re-queued");
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_add_pending_and_candidates() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));
        tracker.add_pending("j3".into(), build_job(peer, vec![]));

        let candidates = tracker.candidates_for_worker(None, None);
        assert_eq!(candidates.len(), 3);
        assert_eq!(tracker.pending_count(), 3);
    }

    #[test]
    fn test_candidates_filtered_by_peer() {
        let mut tracker = JobTracker::new();
        let peer_a = OrganizationId::now_v7();
        let peer_b = OrganizationId::now_v7();
        tracker.add_pending("ja".into(), eval_job(peer_a));
        tracker.add_pending("jb".into(), eval_job(peer_b));

        let mut authorized = HashSet::new();
        authorized.insert(peer_a);

        let candidates = tracker.candidates_for_worker(Some(&authorized), None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].job_id, "ja");
    }

    #[test]
    fn test_candidates_filtered_by_architecture() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        // x86_64 build
        tracker.add_pending(
            "x86".into(),
            build_job_arch(peer, vec![], "x86_64-linux", vec![]),
        );
        // aarch64 build
        tracker.add_pending(
            "arm".into(),
            build_job_arch(peer, vec![], "aarch64-linux", vec![]),
        );
        // builtin builds run anywhere
        tracker.add_pending(
            "any".into(),
            build_job_arch(peer, vec![], "builtin", vec![]),
        );

        let x86_caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let candidates = tracker.candidates_for_worker(None, Some(&x86_caps));
        let mut ids: Vec<_> = candidates.iter().map(|c| c.job_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["any".to_string(), "x86".to_string()]);
    }

    #[test]
    fn fetch_flake_job_requires_fetch_capability() {
        // Regression guard for #252: a FlakeJob carrying FetchFlake clones a
        // repository (over SSH) and so must only be offered to fetch-capable
        // workers - the server only sends SSH credentials to those. A worker
        // without `fetch` (e.g. eval+build only) previously received the job
        // and failed with "authentication required but no callback set".
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), fetch_eval_job(peer));

        let no_fetch = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&no_fetch), &JobKind::Flake, &*p, &inst)
                .is_none(),
            "worker without fetch must not receive a fetch flake job"
        );
        assert_eq!(tracker.pending_count(), 1);

        let with_fetch = WorkerCaps {
            fetch: true,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        assert!(
            tracker
                .take_best_of_kind("w2", None, Some(&with_fetch), &JobKind::Flake, &*p, &inst)
                .is_some(),
            "fetch-capable worker must receive the fetch flake job"
        );
    }

    #[test]
    fn cached_eval_job_runs_without_fetch_capability() {
        // Eval-only follow-up jobs (no FetchFlake task) read an already-cached
        // source and must remain servable by workers that lack `fetch`.
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));

        let no_fetch = WorkerCaps {
            fetch: false,
            architectures: vec![],
            system_features: vec![],
            ..Default::default()
        };
        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&no_fetch), &JobKind::Flake, &*p, &inst)
                .is_some(),
            "cached eval job must run on a worker without fetch"
        );
    }

    #[test]
    fn test_take_best_of_kind_skips_wrong_arch() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending(
            "arm".into(),
            build_job_arch(peer, vec![], "aarch64-linux", vec![]),
        );
        let x86_caps = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        // Worker requesting Build → arm-only build is filtered out → no assignment.
        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        let assignment =
            tracker.take_best_of_kind("w1", None, Some(&x86_caps), &JobKind::Build, &*p, &inst);
        assert!(assignment.is_none());
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_take_best_of_kind_requires_features() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending(
            "kvm".into(),
            build_job_arch(peer, vec![], "x86_64-linux", vec!["kvm".into()]),
        );
        let no_kvm = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
            ..Default::default()
        };
        let with_kvm = WorkerCaps {
            fetch: false,
            architectures: vec!["x86_64-linux".into()],
            system_features: vec!["kvm".into()],
            ..Default::default()
        };
        // Cached score so the build clears the negative-total dispatch gate.
        for w in ["w1", "w2"] {
            tracker.record_scores(
                w,
                vec![CandidateScore { job_id: "kvm".into(), missing_count: 0, missing_nar_size: 0 }],
            );
        }
        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        // Worker without kvm - no assignment.
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&no_kvm), &JobKind::Build, &*p, &inst)
                .is_none()
        );
        // Worker with kvm - assigned.
        assert!(
            tracker
                .take_best_of_kind("w2", None, Some(&with_kvm), &JobKind::Build, &*p, &inst)
                .is_some()
        );
    }

    #[test]
    fn test_record_scores_then_request_assigns_best() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending(
            "j1".into(),
            build_job(
                peer,
                vec![RequiredPath {
                    path: "/nix/store/foo".into(),
                    cache_info: None,
                }],
            ),
        );

        // Record scores, then request - assignment uses the score to pick.
        tracker.record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
            }],
        );
        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn fair_share_quiet_org_wins_over_busy_org() {
        // #111: org A floods the queue and already has builds running; org B is
        // quiet. With the resource-aware policy the next build must go to B so a
        // busy tenant cannot starve a quiet one.
        let mut tracker = JobTracker::new();
        let org_a = OrganizationId::now_v7();
        let org_b = OrganizationId::now_v7();
        let p = score::policy_by_name("resource-aware");
        // Non-zero typical build time so active builds carry work-weight even
        // without per-build history, making org_work_share well-defined.
        let inst = score::InstanceContext {
            build_time_ms: score::Windowed { w1h: 60_000.0, ..Default::default() },
            ..Default::default()
        };

        // Seed several active builds for org A. Each is fully cached on its
        // worker so it clears the negative-total dispatch gate.
        for i in 0..5 {
            let id = format!("a_active_{i}");
            tracker.add_pending(id.clone(), build_job(org_a, vec![]));
            tracker.record_scores(
                "wa",
                vec![CandidateScore { job_id: id, missing_count: 0, missing_nar_size: 0 }],
            );
            tracker.take_best_of_kind("wa", None, None, &JobKind::Build, &*p, &inst);
        }
        assert_eq!(tracker.active_count(), 5);

        // One pending build each for A and B, both cached on the requesting worker.
        tracker.add_pending("a_pending".into(), build_job(org_a, vec![]));
        tracker.add_pending("b_pending".into(), build_job(org_b, vec![]));
        for id in ["a_pending", "b_pending"] {
            tracker.record_scores(
                "wb",
                vec![CandidateScore { job_id: id.into(), missing_count: 0, missing_nar_size: 0 }],
            );
        }

        let assignment = tracker
            .take_best_of_kind("wb", None, None, &JobKind::Build, &*p, &inst)
            .expect("a build must be assigned");
        assert_eq!(
            assignment.job_id, "b_pending",
            "quiet org B must win over busy org A"
        );
    }

    #[test]
    fn unscored_build_is_gated_until_scored() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending(
            "j1".into(),
            build_job(
                peer,
                vec![RequiredPath { path: "/nix/store/foo".into(), cache_info: None }],
            ),
        );

        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();

        // No scores yet: the build's total is negative (RescoreWaitRule), so the
        // negative-total gate idles the worker and leaves the job pending.
        assert!(tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst).is_none());
        assert_eq!(tracker.pending_count(), 1);

        // Once the worker reports it is fully cached, the build clears the gate.
        tracker.record_scores(
            "w1",
            vec![CandidateScore { job_id: "j1".into(), missing_count: 0, missing_nar_size: 0 }],
        );
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst);
        assert_eq!(assignment.unwrap().job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn dispatch_skips_all_negative() {
        // The only eligible build is unscored (missing_nar_size None, rescore 0),
        // so RescoreWaitRule drives its total to -1000 with nothing to offset it.
        // The negative-total gate must idle the worker, leaving the job pending.
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), build_job(peer, vec![]));

        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        assert!(tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst).is_none());
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn dispatch_picks_non_negative() {
        // A fully-cached build (missing_nar_size Some(0)) earns the MissingNarSize
        // bonus and avoids the rescore penalty, so its total is >= 0 and it is
        // dispatched.
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), build_job(peer, vec![]));
        tracker.record_scores(
            "w1",
            vec![CandidateScore { job_id: "j1".into(), missing_count: 0, missing_nar_size: 0 }],
        );

        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &*p, &inst);
        assert_eq!(assignment.expect("non-negative build must dispatch").job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_release_to_pending_after_rejection() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));

        // Assign it.
        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &*p, &inst);
        assert!(assignment.is_some());
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);

        // Release it back.
        tracker.release_to_pending("j1");
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.active_count(), 0);

        // Should reappear in candidates.
        let candidates = tracker.candidates_for_worker(None, None);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_worker_disconnected_requeues() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));

        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        assert_eq!(tracker.active_count(), 2);
        assert_eq!(tracker.pending_count(), 0);

        let orphaned = tracker.worker_disconnected("w1");
        assert_eq!(orphaned.len(), 2);
        assert_eq!(tracker.pending_count(), 2);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_take_empty_required() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        // Job with required paths - should NOT be taken.
        tracker.add_pending(
            "j1".into(),
            build_job(
                peer,
                vec![RequiredPath {
                    path: "/nix/store/x".into(),
                    cache_info: None,
                }],
            ),
        );
        // Job with no required paths - should be taken.
        tracker.add_pending("j2".into(), eval_job(peer));

        let p = score::policy_by_name("simple");
        let inst = score::InstanceContext::default();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &*p, &inst);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j2");
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_aborts_only_revoked_org() {
        let mut tracker = JobTracker::new();
        let org_a = OrganizationId::now_v7();
        let org_b = OrganizationId::now_v7();
        tracker.add_pending("ja1".into(), eval_job(org_a));
        tracker.add_pending("ja2".into(), eval_job(org_a));
        tracker.add_pending("jb1".into(), eval_job(org_b));

        // Assign all three to worker w1.
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        assert_eq!(tracker.active_jobs().count(), 3);

        // Revoke only org_a.
        let revoked = HashSet::from([org_a]);
        let aborted = tracker.drain_peer_jobs_on_worker("w1", &revoked);
        aborted.iter().for_each(|id| assert!(id.starts_with("ja")));
        assert_eq!(aborted.len(), 2);

        // org_b job is still active; org_a jobs are back in pending.
        assert_eq!(tracker.active_jobs().count(), 1);
        assert_eq!(tracker.pending_count(), 2);
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_empty_revoked() {
        let mut tracker = JobTracker::new();
        let org_a = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(org_a));
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );

        let aborted = tracker.drain_peer_jobs_on_worker("w1", &HashSet::new());
        assert!(aborted.is_empty());
        assert_eq!(tracker.active_jobs().count(), 1);
    }

    #[test]
    fn test_contains_job_both_maps() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        assert!(tracker.contains_job("j1"));
        assert!(!tracker.contains_job("j2"));

        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        // Now in active, not pending - should still be "contained".
        assert!(tracker.contains_job("j1"));
    }

    #[test]
    fn remove_job_drops_pending_entry() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        assert!(tracker.contains_job("j1"));
        tracker.remove_job("j1");
        assert!(!tracker.contains_job("j1"));
    }

    #[test]
    fn remove_job_drops_active_entry() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.take_best_of_kind(
            "w1",
            None,
            None,
            &JobKind::Flake,
            &*score::policy_by_name("simple"),
            &score::InstanceContext::default(),
        );
        assert!(tracker.contains_job("j1"));
        tracker.remove_job("j1");
        assert!(!tracker.contains_job("j1"));
    }

    #[test]
    fn remove_job_unknown_id_is_noop() {
        let mut tracker = JobTracker::new();
        tracker.remove_job("does-not-exist");
    }

    #[test]
    fn pending_snapshot_reports_kind_and_org() {
        let mut tracker = JobTracker::new();
        let org = OrganizationId::now_v7();
        tracker.add_pending("eval:1".into(), eval_job(org));
        let snap = tracker.pending_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].kind, 0);
        assert_eq!(snap[0].organization, org);
        assert!(snap[0].build_id.is_none());
    }

    #[test]
    fn cached_followup_rewrites_source_and_tasks() {
        let peer = OrganizationId::now_v7();
        let PendingJob::Eval(original) = fetch_eval_job(peer) else { unreachable!() };

        let follow = original.cached_followup("/nix/store/abc-source".into());

        assert_eq!(
            follow.job.tasks,
            vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations]
        );
        match &follow.job.source {
            FlakeSource::Cached { store_path } => assert_eq!(store_path, "/nix/store/abc-source"),
            other => panic!("expected Cached, got {other:?}"),
        }
        assert_eq!(follow.evaluation_id, original.evaluation_id);
        assert_eq!(follow.peer_id, original.peer_id);
        assert_eq!(follow.repository, original.repository);
        assert_eq!(follow.required_paths.len(), 1);
        assert!(follow.required_paths.iter().any(|p| p.path == "/nix/store/abc-source"));
    }

    #[test]
    fn bump_rescore_increments_pending_only() {
        let mut tracker = JobTracker::new();
        let peer = OrganizationId::now_v7();
        tracker.add_pending("build:1".into(), build_job(peer, vec![]));

        tracker.bump_rescore_counts();
        tracker.bump_rescore_counts();
        assert_eq!(tracker.rescore_count_of("build:1"), 2);

        tracker.assign_pending("worker", "build:1");
        tracker.bump_rescore_counts();
        assert_eq!(tracker.rescore_count_of("build:1"), 0, "active job not bumped");
    }

    #[test]
    fn is_fetch_only_true_only_for_fetch_task_alone() {
        let fetch_only = FlakeJob {
            tasks: vec![FlakeTask::FetchFlake],
            source: FlakeSource::Repository { url: "u".into(), commit: "c".into() },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
        };
        assert!(is_fetch_only(&fetch_only));

        let bundled = FlakeJob {
            tasks: vec![FlakeTask::FetchFlake, FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations],
            ..fetch_only.clone()
        };
        assert!(!is_fetch_only(&bundled));

        let cached = FlakeJob {
            tasks: vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations],
            ..fetch_only.clone()
        };
        assert!(!is_fetch_only(&cached));
    }
}
