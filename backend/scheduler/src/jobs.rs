/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pending and active job tracking.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use gradient_core::types::proto::{
    BuildJob, CandidateScore, FlakeJob, Job, JobCandidate, JobKind, RequiredPath,
};

use crate::policy::{JobContext, Policy, WorkerContext};

#[derive(Debug, Clone)]
pub struct PendingEvalJob {
    pub evaluation_id: Uuid,
    pub project_id: Option<Uuid>,
    /// Peer (org/cache/proxy) that owns this job. Workers must be authorized
    /// for this peer to receive the job offer.
    pub peer_id: Uuid,
    pub commit_id: Uuid,
    pub repository: String,
    pub job: FlakeJob,
    pub required_paths: Vec<RequiredPath>,
    /// `evaluation.updated_at` at the time this job was dispatched.
    /// Used by the scoring policy to prefer evaluations that have waited longer.
    pub queued_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone)]
pub struct PendingBuildJob {
    pub build_id: Uuid,
    pub evaluation_id: Uuid,
    /// Peer (org/cache/proxy) that owns this job.
    pub peer_id: Uuid,
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
    /// `build.updated_at` at the time this job was dispatched to the tracker.
    /// Used by the scoring policy to prefer builds that have waited longer.
    pub queued_at: chrono::NaiveDateTime,
}

/// A connected worker's build-relevant capabilities, used to gate which
/// build jobs are eligible for assignment.
#[derive(Debug, Clone, Default)]
pub struct WorkerBuildCaps {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
}

impl WorkerBuildCaps {
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

    pub fn peer_id(&self) -> Uuid {
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

    pub fn evaluation_id(&self) -> Uuid {
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
}

pub struct Assignment {
    pub job_id: String,
    pub job: Job,
    /// Organization UUID that owns this job — used for credential lookup.
    pub peer_id: Uuid,
}

/// Returns true when `job` is either an eval job (no arch constraint) or a
/// build job whose architecture/features the worker can satisfy. If `caps` is
/// `None`, capability checks are skipped (used for tests / open mode).
fn job_eligible_for_caps(job: &PendingJob, caps: Option<&WorkerBuildCaps>) -> bool {
    match (job, caps) {
        // No capability info known → don't block (legacy behaviour for callers
        // that don't supply caps, e.g. unit tests for unrelated logic).
        (_, None) => true,
        // Eval jobs aren't gated by build caps.
        (PendingJob::Eval(_), Some(_)) => true,
        (PendingJob::Build(j), Some(c)) => c.can_build(&j.architecture, &j.required_features),
    }
}

/// Per-job score submitted by a worker after checking its local store.
#[derive(Debug, Clone, Default)]
pub struct WorkerJobScore {
    /// Number of required store paths not present in the worker's store.
    pub missing_count: u32,
    /// Total uncompressed NAR size (bytes) of the missing paths.
    pub missing_nar_size: u64,
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
        self.pending.insert(job_id, job);
        candidate
    }

    /// Returns all pending job candidates that the worker is authorized to receive
    /// AND can execute. `caps` filters build jobs to those matching the worker's
    /// architectures and system features; eval jobs are always eligible.
    /// Pass `None` for `authorized` to disable peer filtering (open mode).
    /// Pass `None` for `caps` to disable capability filtering (e.g. eval-only worker).
    pub fn candidates_for_worker(
        &self,
        authorized: Option<&HashSet<Uuid>>,
        caps: Option<&WorkerBuildCaps>,
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
    /// assigns jobs in response to an explicit `RequestJob` — scores just
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
    /// This is the ONLY assignment path — the server never assigns without
    /// an explicit `RequestJob` from the worker.
    pub fn take_best_of_kind(
        &mut self,
        peer_id: &str,
        authorized: Option<&HashSet<Uuid>>,
        caps: Option<&WorkerBuildCaps>,
        kind: &JobKind,
        policy: &Policy,
    ) -> Option<Assignment> {
        let worker_scores = self.scores.get(peer_id);

        let worker_ctx = caps.map(|c| WorkerContext {
            architectures: &c.architectures,
            system_features: &c.system_features,
        });
        let empty_archs: Vec<String> = vec![];
        let empty_feats: Vec<String> = vec![];
        let fallback_ctx = WorkerContext {
            architectures: &empty_archs,
            system_features: &empty_feats,
        };
        let worker_ctx = worker_ctx.as_ref().unwrap_or(&fallback_ctx);

        let job_id = self
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
            .max_by(|(id_a, job_a), (id_b, job_b)| {
                let score_a = {
                    let s = worker_scores.and_then(|ws| ws.get(id_a.as_str()));
                    let ctx = JobContext {
                        job: job_a,
                        missing_count: s.map(|s| s.missing_count),
                        missing_nar_size: s.map(|s| s.missing_nar_size),
                        dependency_count: job_a.dependency_count(),
                        queued_at: job_a.queued_at(),
                    };
                    policy.score(&ctx, worker_ctx)
                };
                let score_b = {
                    let s = worker_scores.and_then(|ws| ws.get(id_b.as_str()));
                    let ctx = JobContext {
                        job: job_b,
                        missing_count: s.map(|s| s.missing_count),
                        missing_nar_size: s.map(|s| s.missing_nar_size),
                        dependency_count: job_b.dependency_count(),
                        queued_at: job_b.queued_at(),
                    };
                    policy.score(&ctx, worker_ctx)
                };
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Tie-break by job_id for determinism.
                    .then_with(|| id_b.cmp(id_a))
            })
            .map(|(id, _)| id.clone())?;
        self.assign_pending(peer_id, &job_id)
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

    /// Move all active jobs assigned to `worker_id` that belong to any of
    /// `revoked_peers` back to the pending queue.  Returns the job IDs so the
    /// caller can send `AbortJob` messages to the worker.
    pub fn drain_peer_jobs_on_worker(
        &mut self,
        worker_id: &str,
        revoked_peers: &HashSet<Uuid>,
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

    /// Iterate over active jobs: yields `(job_id, worker_id, &PendingJob)`.
    pub fn active_jobs(&self) -> impl Iterator<Item = (&str, &str, &PendingJob)> {
        self.active
            .iter()
            .map(|(job_id, (worker_id, job))| (job_id.as_str(), worker_id.as_str(), job))
    }

    /// Remove all pending (unassigned) jobs belonging to a given evaluation.
    pub fn remove_pending_for_evaluation(&mut self, evaluation_id: Uuid) {
        self.pending
            .retain(|_, job| job.evaluation_id() != evaluation_id);
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use gradient_core::types::proto::{BuildJob, BuildTask, FlakeJob, FlakeTask};

    fn eval_job(peer: Uuid) -> PendingJob {
        PendingJob::Eval(PendingEvalJob {
            evaluation_id: Uuid::new_v4(),
            project_id: None,
            peer_id: peer,
            commit_id: Uuid::new_v4(),
            repository: "https://example.com/repo".into(),
            job: FlakeJob {
                tasks: vec![FlakeTask::EvaluateDerivations],
                repository: "https://example.com/repo".into(),
                commit: "abc123".into(),
                wildcards: vec!["*".into()],
                timeout_secs: None,
                sign: None,
            },
            required_paths: vec![],
            queued_at: chrono::Utc::now().naive_utc(),
        })
    }

    fn build_job(peer: Uuid, required: Vec<RequiredPath>) -> PendingJob {
        build_job_arch(peer, required, "x86_64-linux", vec![])
    }

    fn build_job_arch(
        peer: Uuid,
        required: Vec<RequiredPath>,
        architecture: &str,
        required_features: Vec<String>,
    ) -> PendingJob {
        PendingJob::Build(PendingBuildJob {
            build_id: Uuid::new_v4(),
            evaluation_id: Uuid::new_v4(),
            peer_id: peer,
            job: BuildJob {
                builds: vec![BuildTask {
                    build_id: Uuid::new_v4().to_string(),
                    drv_path: "/nix/store/abc.drv".into(),
                }],
                compress: None,
                sign: None,
            },
            required_paths: required,
            architecture: architecture.into(),
            required_features,
            dependency_count: 0,
            queued_at: chrono::Utc::now().naive_utc(),
        })
    }

    #[test]
    fn test_add_pending_and_candidates() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
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
        let peer_a = Uuid::new_v4();
        let peer_b = Uuid::new_v4();
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
        let peer = Uuid::new_v4();
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

        let x86_caps = WorkerBuildCaps {
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
        };
        let candidates = tracker.candidates_for_worker(None, Some(&x86_caps));
        let mut ids: Vec<_> = candidates.iter().map(|c| c.job_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["any".to_string(), "x86".to_string()]);
    }

    #[test]
    fn test_take_best_of_kind_skips_wrong_arch() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending(
            "arm".into(),
            build_job_arch(peer, vec![], "aarch64-linux", vec![]),
        );
        let x86_caps = WorkerBuildCaps {
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
        };
        // Worker requesting Build → arm-only build is filtered out → no assignment.
        let p = Policy::default_build_policy();
        let assignment = tracker.take_best_of_kind("w1", None, Some(&x86_caps), &JobKind::Build, &p);
        assert!(assignment.is_none());
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_take_best_of_kind_requires_features() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending(
            "kvm".into(),
            build_job_arch(peer, vec![], "x86_64-linux", vec!["kvm".into()]),
        );
        let no_kvm = WorkerBuildCaps {
            architectures: vec!["x86_64-linux".into()],
            system_features: vec![],
        };
        let with_kvm = WorkerBuildCaps {
            architectures: vec!["x86_64-linux".into()],
            system_features: vec!["kvm".into()],
        };
        let p = Policy::default_build_policy();
        // Worker without kvm — no assignment.
        assert!(
            tracker
                .take_best_of_kind("w1", None, Some(&no_kvm), &JobKind::Build, &p)
                .is_none()
        );
        // Worker with kvm — assigned.
        assert!(
            tracker
                .take_best_of_kind("w2", None, Some(&with_kvm), &JobKind::Build, &p)
                .is_some()
        );
    }

    #[test]
    fn test_record_scores_then_request_assigns_best() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
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

        // Record scores, then request — assignment uses the score to pick.
        tracker.record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
            }],
        );
        let p = Policy::default_build_policy();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &p);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_request_without_scores_still_assigns() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
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

        // No scores recorded — take_best_of_kind still assigns (unscored = MAX).
        let p = Policy::default_build_policy();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Build, &p);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j1");
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_release_to_pending_after_rejection() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));

        // Assign it.
        let p = Policy::default_build_policy();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &p);
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
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));
        tracker.add_pending("j2".into(), eval_job(peer));

        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());
        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());
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
        let peer = Uuid::new_v4();
        // Job with required paths — should NOT be taken.
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
        // Job with no required paths — should be taken.
        tracker.add_pending("j2".into(), eval_job(peer));

        let p = Policy::default_build_policy();
        let assignment = tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &p);
        assert!(assignment.is_some());
        assert_eq!(assignment.unwrap().job_id, "j2");
    }

    #[test]
    fn test_drain_peer_jobs_on_worker_aborts_only_revoked_org() {
        let mut tracker = JobTracker::new();
        let org_a = Uuid::new_v4();
        let org_b = Uuid::new_v4();
        tracker.add_pending("ja1".into(), eval_job(org_a));
        tracker.add_pending("ja2".into(), eval_job(org_a));
        tracker.add_pending("jb1".into(), eval_job(org_b));

        // Assign all three to worker w1.
        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());
        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());
        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());
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
        let org_a = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(org_a));
        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());

        let aborted = tracker.drain_peer_jobs_on_worker("w1", &HashSet::new());
        assert!(aborted.is_empty());
        assert_eq!(tracker.active_jobs().count(), 1);
    }

    #[test]
    fn test_contains_job_both_maps() {
        let mut tracker = JobTracker::new();
        let peer = Uuid::new_v4();
        tracker.add_pending("j1".into(), eval_job(peer));
        assert!(tracker.contains_job("j1"));
        assert!(!tracker.contains_job("j2"));

        tracker.take_best_of_kind("w1", None, None, &JobKind::Flake, &Policy::default_build_policy());
        // Now in active, not pending — should still be "contained".
        assert!(tracker.contains_job("j1"));
    }
}
